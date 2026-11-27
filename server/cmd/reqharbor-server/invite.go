package main

import (
	"context"
	"flag"
	"fmt"
	"io"
	"text/tabwriter"
	"time"

	"github.com/mihajlopopara/reqharbor/server/internal/accounts"
	"github.com/mihajlopopara/reqharbor/server/internal/clock"
	"github.com/mihajlopopara/reqharbor/server/internal/store"
)

const inviteUsage = `usage: reqharbor-server invite <command> [flags]

  create [--email E] [--uses 1] [--ttl 168h]   prints a code XXXX-XXXX-XXXX-XXXX
  list
  revoke ID
`

func invite(args, environ []string, stdout, stderr io.Writer) int {
	if len(args) == 0 {
		fmt.Fprint(stderr, inviteUsage)
		return 2
	}
	sub, args := args[0], args[1:]
	fs := flag.NewFlagSet("invite "+sub, flag.ContinueOnError)
	fs.SetOutput(stderr)
	var email *string
	var uses *int64
	var ttl *time.Duration
	switch sub {
	case "create":
		email = fs.String("email", "", "only this email may use the code")
		uses = fs.Int64("uses", 1, "how many accounts the code may create")
		ttl = fs.Duration("ttl", 168*time.Hour, "how long the code works")
	case "list", "revoke":
	default:
		fmt.Fprintf(stderr, "unknown invite command %q\n\n%s", sub, inviteUsage)
		return 2
	}
	if err := fs.Parse(args); err != nil {
		return 2
	}
	if sub == "revoke" && fs.NArg() != 1 {
		fmt.Fprintln(stderr, "invite revoke: give the invite ID from `invite list`")
		return 2
	}
	e, code := openCLI(environ, stderr)
	if e == nil {
		return code
	}
	defer e.close()

	switch sub {
	case "create":
		if *ttl <= 0 {
			fmt.Fprintln(stderr, "invite create: --ttl must be positive")
			return 2
		}
		var inv *accounts.Invite
		err := e.write(func(tx store.WriteTx) error {
			var err error
			inv, err = accounts.NewInvite(tx, *email, *uses, *ttl, e.now())
			return err
		})
		if err != nil {
			return fail(stderr, err)
		}
		fmt.Fprintf(stdout, "%s\nInvite %s: %d use(s), expires %s. Enter the code on the sign-in page under Create an account.\n",
			inv.Code, inv.ID, *uses, inv.Expires.UTC().Format("2006-01-02 15:04 UTC"))
		return 0
	case "list":
		var list []store.OneTimeToken
		err := e.store.Read(context.Background(), func(tx store.ReadTx) error {
			var err error
			list, err = tx.OneTimeTokens(store.PurposeSignupInvite)
			return err
		})
		if err != nil {
			return fail(stderr, err)
		}
		now := clock.Millis(e.now())
		tw := tabwriter.NewWriter(stdout, 0, 4, 2, ' ', 0)
		fmt.Fprintln(tw, "ID\tEMAIL\tUSES\tEXPIRES\tSTATUS")
		for _, t := range list {
			status := "active"
			switch {
			case t.RevokedAt != nil:
				status = "revoked"
			case t.Uses >= t.MaxUses:
				status = "used"
			case t.ExpiresAt <= now:
				status = "expired"
			}
			restrict := "-"
			if t.Email != nil {
				restrict = *t.Email
			}
			fmt.Fprintf(tw, "%s\t%s\t%d/%d\t%s\t%s\n", t.ID, restrict, t.Uses, t.MaxUses,
				time.UnixMilli(t.ExpiresAt).UTC().Format("2006-01-02 15:04"), status)
		}
		tw.Flush()
		return 0
	case "revoke":
		id := fs.Arg(0)
		var ok bool
		err := e.write(func(tx store.WriteTx) error {
			var err error
			ok, err = tx.RevokeOneTimeToken(id, clock.Millis(e.now()))
			return err
		})
		if err != nil {
			return fail(stderr, err)
		}
		if !ok {
			return fail(stderr, fmt.Errorf("no active invite %q", id))
		}
		fmt.Fprintf(stdout, "invite %s revoked\n", id)
		return 0
	}
	return 2
}
