package mail

import (
	"context"
	"log/slog"
	netmail "net/mail"
	"strings"
	"sync"
	"time"

	"github.com/mihajlopopara/reqharbor/server/internal/clock"
	"github.com/mihajlopopara/reqharbor/server/internal/ratelimit"
)

// QueueSize bounds the jobs waiting for the sender; more are dropped and logged.
const QueueSize = 256

// Outbox queues email for one background sender (Run), so a hosted page never waits for the SMTP
// server and takes the same time whether or not it sends an email. It applies the email caps of
// §5.9: per recipient address across every purpose, and for the whole server. A message over a
// cap is silently not sent; the log says email_suppressed with the purpose, never the address.
//
// Two kinds of job: Send queues a finished message (the caps apply at once, and Send reports
// whether the message will go out); Later queues a function that the sender runs to build the
// message (database work included), so the request that queued it does the same work whatever
// the function finds; the caps then apply when it has built a message.
type Outbox struct {
	sender Sender
	from   *netmail.Address
	clock  clock.Clock
	logger *slog.Logger

	PerRecipient *ratelimit.Limiter // REQHARBOR_RATE_EMAIL_PER_RECIPIENT_PER_HOUR
	Global       *ratelimit.Limiter // REQHARBOR_RATE_EMAIL_PER_HOUR

	// slots holds one token per queued job; a job takes its slot before any cap is consumed,
	// so a full queue never spends a cap, and the send to queue below never blocks.
	slots chan struct{}
	queue chan job

	mu      sync.Mutex
	pending int
	idle    *sync.Cond
}

type job struct {
	purpose string
	to      string
	data    []byte
	build   func(ctx context.Context) (*Message, error) // a Later job
}

// OutboxConfig are the inputs of NewOutbox.
type OutboxConfig struct {
	Sender              Sender
	From                *netmail.Address
	Clock               clock.Clock
	Logger              *slog.Logger
	PerRecipientPerHour int64
	PerHour             int64
	Capacity            int // 0 means QueueSize
}

// NewOutbox returns an outbox; Run must be started for messages to leave.
func NewOutbox(c OutboxConfig) *Outbox {
	n := c.Capacity
	if n <= 0 {
		n = QueueSize
	}
	o := &Outbox{
		sender: c.Sender, from: c.From, clock: c.Clock, logger: c.Logger,
		PerRecipient: ratelimit.New(c.Clock, c.PerRecipientPerHour, time.Hour),
		Global:       ratelimit.New(c.Clock, c.PerHour, time.Hour),
		slots:        make(chan struct{}, n),
		queue:        make(chan job, n),
	}
	o.idle = sync.NewCond(&o.mu)
	return o
}

// reserve takes a queue slot; false when the queue is full.
func (o *Outbox) reserve() bool {
	select {
	case o.slots <- struct{}{}:
		o.mu.Lock()
		o.pending++
		o.mu.Unlock()
		return true
	default:
		return false
	}
}

// release gives a slot back (its job is done or was never queued).
func (o *Outbox) release() {
	<-o.slots
	o.mu.Lock()
	o.pending--
	if o.pending == 0 {
		o.idle.Broadcast()
	}
	o.mu.Unlock()
}

// allow applies the per-recipient cap, then the server cap.
func (o *Outbox) allow(purpose, to string) bool {
	if ok, _ := o.PerRecipient.Allow("rcpt:" + to); !ok {
		o.logger.Info("email_suppressed", "purpose", purpose, "cap", "per_recipient")
		return false
	}
	if ok, _ := o.Global.Allow("all"); !ok {
		o.logger.Warn("email_suppressed", "purpose", purpose, "cap", "server")
		return false
	}
	return true
}

// Send queues m for delivery and returns at once. purpose names the email in logs
// (signup_code, account_exists, reset_link, reset_no_password). It reports whether the message
// was queued: false for an invalid recipient, a cap or a full queue. Callers must not show the
// difference; a signup code that was not queued must not be usable.
func (o *Outbox) Send(purpose string, m Message) bool {
	m.To = strings.ToLower(strings.TrimSpace(m.To))
	data, err := render(o.from, m, o.clock.Now())
	if err != nil {
		o.logger.Warn("email_invalid", "purpose", purpose, "err", err.Error())
		return false
	}
	if !o.reserve() {
		o.logger.Warn("email_dropped", "purpose", purpose, "reason", "queue full")
		return false
	}
	if !o.allow(purpose, m.To) {
		o.release()
		return false
	}
	o.queue <- job{purpose: purpose, to: m.To, data: data}
	return true
}

// Later queues build for the sender, which runs it and sends the message it returns (nil: send
// nothing). The caps apply to that message. It reports false only when the queue is full.
func (o *Outbox) Later(purpose string, build func(ctx context.Context) (*Message, error)) bool {
	if !o.reserve() {
		o.logger.Warn("email_dropped", "purpose", purpose, "reason", "queue full")
		return false
	}
	o.queue <- job{purpose: purpose, build: build}
	return true
}

// Run delivers queued jobs one at a time until ctx ends. Jobs still queued then are dropped; the
// count is logged.
func (o *Outbox) Run(ctx context.Context) {
	for {
		if ctx.Err() != nil {
			n := 0
			for len(o.queue) > 0 {
				<-o.queue
				n++
				o.release()
			}
			if n > 0 {
				o.logger.Warn("emails not sent at shutdown", "count", n)
			}
			return
		}
		select {
		case <-ctx.Done():
		case j := <-o.queue:
			o.deliver(ctx, j)
			o.release()
		}
	}
}

func (o *Outbox) deliver(ctx context.Context, j job) {
	if j.build != nil {
		m, err := j.build(ctx)
		if err != nil {
			if ctx.Err() == nil {
				o.logger.Error("email job failed", "purpose", j.purpose, "err", err.Error())
			}
			return
		}
		if m == nil {
			return
		}
		m.To = strings.ToLower(strings.TrimSpace(m.To))
		data, err := render(o.from, *m, o.clock.Now())
		if err != nil {
			o.logger.Warn("email_invalid", "purpose", j.purpose, "err", err.Error())
			return
		}
		if !o.allow(j.purpose, m.To) {
			return
		}
		j.to, j.data = m.To, data
	}
	start := time.Now()
	if err := o.sender.Send(ctx, o.from.Address, j.to, j.data); err != nil {
		// The error names the step and reply code, never the address or the credentials.
		o.logger.Warn("email_failed", "purpose", j.purpose, "err", err.Error())
		return
	}
	o.logger.Info("email_sent", "purpose", j.purpose, "dur_ms", time.Since(start).Milliseconds())
}

// Flush waits until every queued job was handled (handed to the SMTP server, failed or found
// nothing to send), or ctx ends; it reports whether the queue drained. Tests use it.
func (o *Outbox) Flush(ctx context.Context) bool {
	stop := context.AfterFunc(ctx, func() {
		o.mu.Lock()
		o.idle.Broadcast()
		o.mu.Unlock()
	})
	defer stop()
	o.mu.Lock()
	defer o.mu.Unlock()
	for o.pending > 0 && ctx.Err() == nil {
		o.idle.Wait()
	}
	return o.pending == 0
}

// Limiters returns the caps' limiters, for the sweeper job.
func (o *Outbox) Limiters() []*ratelimit.Limiter {
	return []*ratelimit.Limiter{o.PerRecipient, o.Global}
}
