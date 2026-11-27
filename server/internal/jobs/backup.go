package jobs

import (
	"context"
	"errors"
	"fmt"
	"log/slog"
	"os"
	"path/filepath"
	"regexp"
	"sort"
	"strconv"
	"time"

	"github.com/mihajlopopara/reqharbor/server/internal/clock"
	"github.com/mihajlopopara/reqharbor/server/internal/db"
)

// Scheduled backups (06-go-server.md §6 Backups, S11): every REQHARBOR_BACKUP_INTERVAL, a
// VACUUM INTO snapshot on the backup connection, written with the same private write-then-link
// path as the `backup` command, to backups/reqharbor-YYYYMMDD-HHMMSS.db (UTC), then pruned to the
// newest REQHARBOR_BACKUP_KEEP. Only files with exactly that name are the job's: manual-…,
// pre-migrate-… and anything else in backups/ are never touched.

// BackupRetryDelay is how long after a failed scheduled backup the next attempt runs (or the
// interval, when that is shorter).
const BackupRetryDelay = time.Hour

// BackupPrefix starts the name of every scheduled backup.
const BackupPrefix = "reqharbor-"

// scheduledRe matches the names this job writes: reqharbor-YYYYMMDD-HHMMSS.db, with -N when
// two backups fall in the same second.
var scheduledRe = regexp.MustCompile(`^reqharbor-(\d{8}-\d{6})(?:-(\d{1,4}))?\.db$`)

// Backuper takes one snapshot; *db.DB implements it on its backup connection.
type Backuper interface {
	BackupTo(ctx context.Context, path string) error
	BackupsDir() string
}

// ScheduledBackup is one file of the job.
type ScheduledBackup struct {
	Name string
	At   time.Time // from the name (UTC, second precision)
	Seq  int       // the -N suffix; 0 without one
}

// ScheduledBackups lists the job's files in dir, oldest first. A missing directory is empty.
func ScheduledBackups(dir string) ([]ScheduledBackup, error) {
	entries, err := os.ReadDir(dir)
	if errors.Is(err, os.ErrNotExist) {
		return nil, nil
	}
	if err != nil {
		return nil, err
	}
	var out []ScheduledBackup
	for _, e := range entries {
		m := scheduledRe.FindStringSubmatch(e.Name())
		if m == nil || !e.Type().IsRegular() {
			continue
		}
		at, err := time.ParseInLocation("20060102-150405", m[1], time.UTC)
		if err != nil {
			continue
		}
		seq := 0
		if m[2] != "" {
			seq, _ = strconv.Atoi(m[2])
		}
		out = append(out, ScheduledBackup{Name: e.Name(), At: at, Seq: seq})
	}
	sort.Slice(out, func(i, j int) bool {
		if !out[i].At.Equal(out[j].At) {
			return out[i].At.Before(out[j].At)
		}
		return out[i].Seq < out[j].Seq
	})
	return out, nil
}

// NextBackup is how long to wait from now for the next scheduled backup: at once when there is
// none, otherwise until interval after the newest (0 when that is past; at most one interval when
// the clock moved back).
func NextBackup(now, last time.Time, ran bool, interval time.Duration) time.Duration {
	if !ran {
		return 0
	}
	return min(max(last.Add(interval).Sub(now), 0), interval)
}

// BackupResult describes one scheduled backup.
type BackupResult struct {
	Path   string
	Bytes  int64
	Pruned []string
}

// RunScheduledBackup takes one scheduled backup now, verifies it (PRAGMA quick_check) and prunes
// the job's files down to keep. A failed or unverifiable backup is removed and prunes nothing.
func RunScheduledBackup(ctx context.Context, b Backuper, now time.Time, keep int) (*BackupResult, error) {
	dir := b.BackupsDir()
	base := BackupPrefix + now.UTC().Format("20060102-150405")
	var path string
	for seq := 0; ; seq++ {
		name := base + ".db"
		if seq > 0 {
			name = fmt.Sprintf("%s-%d.db", base, seq+1)
		}
		path = filepath.Join(dir, name)
		if _, err := os.Lstat(path); errors.Is(err, os.ErrNotExist) {
			break
		}
		if seq > 1000 {
			return nil, fmt.Errorf("too many backups named %s-*", base)
		}
	}
	if err := b.BackupTo(ctx, path); err != nil {
		return nil, err
	}
	info, err := db.VerifyBackup(ctx, path)
	if err != nil {
		_ = os.Remove(path)
		return nil, fmt.Errorf("the backup does not verify, removed it: %w", err)
	}
	res := &BackupResult{Path: path, Bytes: info.Bytes}
	res.Pruned, err = PruneScheduledBackups(dir, keep, filepath.Base(path))
	return res, err
}

// PruneScheduledBackups deletes the oldest of the job's files until keep remain, and returns the
// names it deleted. The file named latest (the one just written) is never deleted, even when its
// name sorts oldest because the clock was ahead when the others were written. Nothing else in dir
// is touched.
func PruneScheduledBackups(dir string, keep int, latest string) ([]string, error) {
	if keep < 1 {
		keep = 1
	}
	list, err := ScheduledBackups(dir)
	if err != nil {
		return nil, err
	}
	excess := len(list) - keep
	var pruned []string
	for _, b := range list {
		if excess <= 0 {
			break
		}
		if b.Name == latest {
			continue
		}
		if err := os.Remove(filepath.Join(dir, b.Name)); err != nil && !errors.Is(err, os.ErrNotExist) {
			return pruned, err
		}
		pruned = append(pruned, b.Name)
		excess--
	}
	return pruned, nil
}

// RunBackups takes a scheduled backup whenever one is due, until ctx ends: interval after the
// newest of the job's files (so restarts do not delay or repeat it), or at once when there is
// none. The next run is due interval after the time the last one started (its file's time), so
// the schedule does not drift by the backups' duration. A failed run is retried after min(interval, BackupRetryDelay). Shutdown (ctx) interrupts a
// backup in progress; its temporary file is removed and nothing is left under the final name.
func RunBackups(ctx context.Context, b Backuper, clk clock.Clock, interval time.Duration, keep int, logger *slog.Logger) {
	list, err := ScheduledBackups(b.BackupsDir())
	if err != nil {
		logger.Warn("scheduled backups: cannot list the backups directory", "err", err.Error())
	}
	var wait time.Duration
	if len(list) > 0 {
		wait = NextBackup(clk.Now(), list[len(list)-1].At, true, interval)
	}
	for {
		t := time.NewTimer(wait)
		select {
		case <-ctx.Done():
			t.Stop()
			return
		case <-t.C:
		}
		start := time.Now()
		at := clk.Now()
		res, err := RunScheduledBackup(ctx, b, at, keep)
		if res == nil {
			if ctx.Err() != nil {
				return
			}
			wait = min(interval, BackupRetryDelay)
			logger.Error("scheduled backup failed", "err", err.Error(), "retry_in", wait.String())
			continue
		}
		wait = NextBackup(clk.Now(), at, true, interval)
		logger.Info("scheduled backup", "path", res.Path, "bytes", res.Bytes, "dur_ms", time.Since(start).Milliseconds(),
			"pruned", len(res.Pruned), "keep", keep)
		if err != nil {
			logger.Warn("scheduled backup: pruning old backups failed", "err", err.Error())
		}
	}
}
