package main

import (
	"bufio"
	"context"
	"crypto/rand"
	"encoding/base64"
	"errors"
	"flag"
	"fmt"
	"io"
	"os"
	"strings"
	"text/tabwriter"
	"time"

	"github.com/mihajlopopara/reqharbor/server/internal/accounts"
	"github.com/mihajlopopara/reqharbor/server/internal/auth"
	"github.com/mihajlopopara/reqharbor/server/internal/auth/throttle"
	"github.com/mihajlopopara/reqharbor/server/internal/clock"
	"github.com/mihajlopopara/reqharbor/server/internal/config"
	"github.com/mihajlopopara/reqharbor/server/internal/db"
	"github.com/mihajlopopara/reqharbor/server/internal/store"
	"github.com/mihajlopopara/reqharbor/server/internal/store/sqlite"
)

// stdin is read by --password-stdin; tests replace it.
var stdin io.Reader = os.Stdin

// MaxResetLinkTTL caps `user reset-link --ttl`.
const MaxResetLinkTTL = 7 * 24 * time.Hour

// cliEnv is what the account commands need: the validated configuration and an open store on a
// fully migrated database. They do not take the single-instance lock, so they work next to a
// running `serve` (short transactions with busy_timeout).
type cliEnv struct {
	cfg   *config.Config
	db    *db.DB
	store store.Store
	now   func() time.Time
}

func openCLI(environ []string, stderr io.Writer) (*cliEnv, int) {
	res, err := config.Parse(config.Options{Environ: environ})
	if err != nil {
		fmt.Fprintln(stderr, "reqharbor-server:", err)
		return nil, 2
	}
	d, err := db.Open(res.Config.DataDir, db.Options{})
	if err != nil {
		fmt.Fprintln(stderr, "reqharbor-server:", err)
		return nil, 1
	}
	st, err := d.MigrationStatus(context.Background())
	if err != nil {
		d.Close()
		fmt.Fprintln(stderr, "reqharbor-server:", err)
		return nil, 1
	}
	if len(st.Pending) > 0 {
		d.Close()
		fmt.Fprintf(stderr, "reqharbor-server: the database is at schema v%d and needs v%d; start serve once (or run migrate) first\n", st.Current, st.Latest)
		return nil, 1
	}
	return &cliEnv{cfg: res.Config, db: d, store: sqlite.New(d), now: time.Now}, 0
}

func (e *cliEnv) close() { e.db.Close() }

func (e *cliEnv) write(fn func(tx store.WriteTx) error) error {
	return e.store.Write(context.Background(), fn)
}

// fail prints a command error and returns exit code 1.
func fail(stderr io.Writer, err error) int {
	fmt.Fprintln(stderr, "reqharbor-server:", err)
	return 1
}

func readPassword(stdin io.Reader) (string, error) {
	line, err := bufio.NewReader(stdin).ReadString('\n')
	if err != nil && !errors.Is(err, io.EOF) {
		return "", err
	}
	pw := strings.TrimRight(line, "\r\n")
	if pw == "" {
		return "", errors.New("--password-stdin: no password on standard input")
	}
	return pw, nil
}

const userUsage = `usage: reqharbor-server user <command> [flags]

  list
  create --email E [--name N] [--admin] [--force] [--password-stdin | --reset-link]
  set-password --email E --password-stdin
  reset-link --email E [--ttl 24h]
  set-admin --email E (--on | --off)
  disable --email E          (revokes all sessions)
  enable --email E
  unlock --email E           (clears the password throttle for that email, all IP buckets)
  sessions revoke --email E
`

func user(args, environ []string, stdout, stderr io.Writer) int {
	if len(args) == 0 {
		fmt.Fprint(stderr, userUsage)
		return 2
	}
	sub, args := args[0], args[1:]
	if sub == "sessions" {
		if len(args) == 0 || args[0] != "revoke" {
			fmt.Fprint(stderr, userUsage)
			return 2
		}
		sub, args = "sessions-revoke", args[1:]
	}
	fs := flag.NewFlagSet("user "+sub, flag.ContinueOnError)
	fs.SetOutput(stderr)
	email := fs.String("email", "", "account email")
	var name *string
	var admin, force, pwStdin, resetLink, on, off *bool
	var ttl *time.Duration
	switch sub {
	case "list":
	case "create":
		name = fs.String("name", "", "display name")
		admin = fs.Bool("admin", false, "make the account a server admin")
		force = fs.Bool("force", false, "create it even when REQHARBOR_MAX_USERS is reached")
		pwStdin = fs.Bool("password-stdin", false, "read the password from standard input")
		resetLink = fs.Bool("reset-link", false, "print a link that sets the password")
	case "set-password":
		pwStdin = fs.Bool("password-stdin", false, "read the password from standard input")
	case "reset-link":
		ttl = fs.Duration("ttl", auth.ResetLinkTTL, "how long the link works (at most 168h)")
	case "set-admin":
		on = fs.Bool("on", false, "make admin")
		off = fs.Bool("off", false, "remove admin")
	case "disable", "enable", "unlock", "sessions-revoke":
	default:
		fmt.Fprintf(stderr, "unknown user command %q\n\n%s", sub, userUsage)
		return 2
	}
	if err := fs.Parse(args); err != nil {
		return 2
	}
	if sub != "list" && strings.TrimSpace(*email) == "" {
		fmt.Fprintf(stderr, "user %s: --email is required\n", sub)
		return 2
	}
	// A reset link is a single-use bearer credential printed to a terminal: 24 h by default
	// (§5.7), never longer than MaxResetLinkTTL.
	if ttl != nil && (*ttl <= 0 || *ttl > MaxResetLinkTTL) {
		fmt.Fprintf(stderr, "user reset-link: --ttl must be positive and at most %s\n", MaxResetLinkTTL)
		return 2
	}
	e, code := openCLI(environ, stderr)
	if e == nil {
		return code
	}
	defer e.close()
	addr := accounts.NormalizeEmail(*email)

	switch sub {
	case "list":
		return userList(e, stdout, stderr)
	case "create":
		if *pwStdin == *resetLink {
			fmt.Fprintln(stderr, "user create: give exactly one of --password-stdin and --reset-link")
			return 2
		}
		return userCreate(e, addr, *name, *admin, *force, *pwStdin, stdout, stderr)
	case "set-password":
		if !*pwStdin {
			fmt.Fprintln(stderr, "user set-password: --password-stdin is required")
			return 2
		}
		return userSetPassword(e, addr, stdout, stderr)
	case "reset-link":
		var link string
		err := e.write(func(tx store.WriteTx) error {
			var err error
			link, err = accounts.NewResetLink(tx, e.cfg.PublicURL, addr, *ttl, e.now())
			return err
		})
		if err != nil {
			return fail(stderr, err)
		}
		fmt.Fprintf(stdout, "%s\nThe link works once, for %s. Send it to the account owner privately.\n", link, *ttl)
		return 0
	case "set-admin":
		if *on == *off {
			fmt.Fprintln(stderr, "user set-admin: give exactly one of --on and --off")
			return 2
		}
		err := e.write(func(tx store.WriteTx) error {
			u, err := accounts.UserByEmail(tx, addr)
			if err != nil {
				return err
			}
			return tx.SetUserAdmin(u.ID, *on, clock.Millis(e.now()))
		})
		if err != nil {
			return fail(stderr, err)
		}
		fmt.Fprintf(stdout, "%s admin: %v\n", addr, *on)
		return 0
	case "disable", "enable":
		var revoked int64
		err := e.write(func(tx store.WriteTx) error {
			u, err := accounts.UserByEmail(tx, addr)
			if err != nil {
				return err
			}
			now := clock.Millis(e.now())
			if sub == "enable" {
				return tx.SetUserDisabled(u.ID, nil, now)
			}
			if err := tx.SetUserDisabled(u.ID, &now, now); err != nil {
				return err
			}
			revoked, err = tx.RevokeUserSessions(u.ID, now)
			return err
		})
		if err != nil {
			return fail(stderr, err)
		}
		if sub == "disable" {
			fmt.Fprintf(stdout, "%s disabled; %d sessions revoked\n", addr, revoked)
		} else {
			fmt.Fprintf(stdout, "%s enabled\n", addr)
		}
		return 0
	case "unlock":
		var n int64
		err := e.write(func(tx store.WriteTx) error {
			var err error
			n, err = throttle.ClearEmail(tx, addr)
			return err
		})
		if err != nil {
			return fail(stderr, err)
		}
		fmt.Fprintf(stdout, "%s unlocked (%d throttle entries cleared)\n", addr, n)
		return 0
	case "sessions-revoke":
		var n int64
		err := e.write(func(tx store.WriteTx) error {
			u, err := accounts.UserByEmail(tx, addr)
			if err != nil {
				return err
			}
			n, err = tx.RevokeUserSessions(u.ID, clock.Millis(e.now()))
			return err
		})
		if err != nil {
			return fail(stderr, err)
		}
		fmt.Fprintf(stdout, "%s: %d sessions revoked\n", addr, n)
		return 0
	}
	return 2
}

func userList(e *cliEnv, stdout, stderr io.Writer) int {
	type row struct {
		u      store.User
		method string
	}
	var rows []row
	err := e.store.Read(context.Background(), func(tx store.ReadTx) error {
		us, err := tx.Users()
		if err != nil {
			return err
		}
		for _, u := range us {
			m, err := accounts.MethodLabel(tx, u.ID)
			if err != nil {
				return err
			}
			rows = append(rows, row{u, m})
		}
		return nil
	})
	if err != nil {
		return fail(stderr, err)
	}
	tw := tabwriter.NewWriter(stdout, 0, 4, 2, ' ', 0)
	fmt.Fprintln(tw, "EMAIL\tID\tADMIN\tSTATUS\tSIGNS IN WITH\tCREATED")
	for _, r := range rows {
		status := "active"
		if r.u.DisabledAt != nil {
			status = "disabled"
		}
		admin := "no"
		if r.u.IsAdmin {
			admin = "yes"
		}
		fmt.Fprintf(tw, "%s\t%s\t%s\t%s\t%s\t%s\n", r.u.Email, r.u.ID, admin, status, r.method,
			time.UnixMilli(r.u.CreatedAt).UTC().Format("2006-01-02 15:04"))
	}
	tw.Flush()
	return 0
}

func userCreate(e *cliEnv, email, name string, admin, force, pwStdin bool, stdout, stderr io.Writer) int {
	if !accounts.ValidEmail(email) {
		return fail(stderr, fmt.Errorf("%q is not a valid email address", email))
	}
	var password string
	if pwStdin {
		var err error
		if password, err = readPassword(stdin); err != nil {
			return fail(stderr, err)
		}
		if p := accounts.PasswordProblem(password, email); p != "" {
			return fail(stderr, errors.New(p))
		}
	} else {
		// The owner sets the password with the reset link; until then nobody knows it.
		var b [32]byte
		_, _ = rand.Read(b[:])
		password = base64.RawURLEncoding.EncodeToString(b[:])
	}
	phc, err := auth.NewHasher(e.cfg.Argon2MemoryKiB).Hash(password)
	if err != nil {
		return fail(stderr, err)
	}
	var created *accounts.Created
	var link string
	err = e.write(func(tx store.WriteTx) error {
		var err error
		created, err = accounts.Create(tx, accounts.CreateParams{
			Email: email, Name: strings.TrimSpace(name), EmailTrusted: true, PHC: phc, CLI: true, CLIAdmin: admin, Force: force,
			AdminEmail: e.cfg.AdminEmail, MaxUsers: e.cfg.MaxUsers, Now: e.now(),
		})
		if err != nil {
			return err
		}
		if !pwStdin {
			link, err = accounts.NewResetLink(tx, e.cfg.PublicURL, email, auth.ResetLinkTTL, e.now())
		}
		return err
	})
	switch {
	case errors.Is(err, accounts.ErrEmailInUse):
		return fail(stderr, errors.New("an account with this email already exists"))
	case errors.Is(err, accounts.ErrUserLimit):
		return fail(stderr, errors.New("REQHARBOR_MAX_USERS is reached; add --force to create the account anyway"))
	case err != nil:
		return fail(stderr, err)
	}
	role := "user"
	if created.User.IsAdmin {
		role = "admin"
	}
	fmt.Fprintf(stdout, "created %s %s (%s)\n", role, created.User.Email, created.User.ID)
	if link != "" {
		fmt.Fprintf(stdout, "%s\nThe link sets the password; it works once, for %s.\n", link, auth.ResetLinkTTL)
	}
	return 0
}

func userSetPassword(e *cliEnv, email string, stdout, stderr io.Writer) int {
	password, err := readPassword(stdin)
	if err != nil {
		return fail(stderr, err)
	}
	if p := accounts.PasswordProblem(password, email); p != "" {
		return fail(stderr, errors.New(p))
	}
	phc, err := auth.NewHasher(e.cfg.Argon2MemoryKiB).Hash(password)
	if err != nil {
		return fail(stderr, err)
	}
	var revoked int64
	err = e.write(func(tx store.WriteTx) error {
		u, err := accounts.UserByEmail(tx, email)
		if err != nil {
			return err
		}
		if _, err := tx.PasswordPHC(u.ID); errors.Is(err, store.ErrNotFound) {
			label, lerr := accounts.MethodLabel(tx, u.ID)
			if lerr != nil {
				return lerr
			}
			return fmt.Errorf("this account signs in with %s, so it has no password", label)
		} else if err != nil {
			return err
		}
		now := clock.Millis(e.now())
		if err := tx.SetPassword(u.ID, phc, now); err != nil {
			return err
		}
		if revoked, err = tx.RevokeUserSessions(u.ID, now); err != nil {
			return err
		}
		_, err = throttle.ClearEmail(tx, email)
		return err
	})
	if err != nil {
		return fail(stderr, err)
	}
	fmt.Fprintf(stdout, "password changed for %s; %d sessions revoked\n", email, revoked)
	return 0
}
