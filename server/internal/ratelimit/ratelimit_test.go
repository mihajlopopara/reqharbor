package ratelimit

import (
	"testing"
	"time"

	"github.com/mihajlopopara/reqharbor/server/internal/clock"
)

func TestLimiter_BurstThenSteadyRate(t *testing.T) {
	clk := clock.NewFake(time.Unix(1_800_000_000, 0))
	l := New(clk, 10, time.Hour)
	for i := 0; i < 10; i++ {
		if ok, _ := l.Allow("a"); !ok {
			t.Fatalf("event %d refused", i)
		}
	}
	ok, retry := l.Allow("a")
	if ok || retry != 6*time.Minute {
		t.Fatalf("11th: ok=%v retry=%v", ok, retry)
	}
	if ok, _ := l.Allow("b"); !ok {
		t.Fatal("other key limited")
	}
	clk.Advance(6*time.Minute - time.Second)
	if ok, _ := l.Allow("a"); ok {
		t.Fatal("allowed early")
	}
	clk.Advance(time.Second)
	if ok, _ := l.Allow("a"); !ok {
		t.Fatal("refused after the interval")
	}
	if ok, _ := l.Allow("a"); ok {
		t.Fatal("second event in one interval")
	}
	// A refused event does not consume budget.
	clk.Advance(6 * time.Minute)
	if ok, _ := l.Allow("a"); !ok {
		t.Fatal("refused")
	}
}

func TestLimiter_SweepDropsDrainedKeys(t *testing.T) {
	clk := clock.NewFake(time.Unix(1_800_000_000, 0))
	l := New(clk, 30, time.Hour)
	l.Allow("a")
	l.Allow("b")
	if l.Len() != 2 {
		t.Fatalf("len %d", l.Len())
	}
	clk.Advance(time.Minute)
	l.Allow("b")
	clk.Advance(2*time.Minute + time.Second)
	l.Sweep()
	if l.Len() != 1 {
		t.Fatalf("len %d after sweep", l.Len())
	}
	clk.Advance(time.Hour)
	l.Sweep()
	if l.Len() != 0 {
		t.Fatalf("len %d", l.Len())
	}
}

func TestLimiter_LimitOfOne(t *testing.T) {
	clk := clock.NewFake(time.Unix(0, 0))
	l := New(clk, 1, time.Minute)
	if ok, _ := l.Allow("k"); !ok {
		t.Fatal("first refused")
	}
	if ok, retry := l.Allow("k"); ok || retry != time.Minute {
		t.Fatalf("second: %v %v", ok, retry)
	}
}

func TestLimiter_CheckDoesNotRecord(t *testing.T) {
	clk := clock.NewFake(time.Unix(1_800_000_000, 0))
	l := New(clk, 2, time.Hour)
	for range 5 {
		if ok, _ := l.Check("a"); !ok {
			t.Fatal("Check refused an unused key")
		}
	}
	l.Allow("a")
	l.Allow("a")
	ok, retry := l.Check("a")
	if ok || retry != 30*time.Minute {
		t.Fatalf("Check after the budget: %v %v", ok, retry)
	}
	if ok, _ := l.Allow("a"); ok {
		t.Fatal("Allow after the budget")
	}
	clk.Advance(30 * time.Minute)
	if ok, _ := l.Check("a"); !ok {
		t.Fatal("Check after the interval")
	}
}
