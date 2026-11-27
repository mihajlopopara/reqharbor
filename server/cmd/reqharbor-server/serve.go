package main

import (
	"context"
	"errors"
	"flag"
	"fmt"
	"io"
	"log/slog"
	"os/signal"
	"syscall"

	"github.com/mihajlopopara/reqharbor/server/internal/app"
	"github.com/mihajlopopara/reqharbor/server/internal/config"
)

func serve(args, environ []string, _ io.Writer, stderr io.Writer) int {
	fs := flag.NewFlagSet("serve", flag.ContinueOnError)
	fs.SetOutput(stderr)
	if err := fs.Parse(args); err != nil {
		return 2
	}
	res, err := config.Parse(config.Options{Environ: environ})
	if err != nil {
		fmt.Fprintln(stderr, "reqharbor-server:", err)
		return 2
	}
	cfg := res.Config
	logger := newLogger(cfg, stderr)
	for _, w := range res.Warnings {
		logger.Warn(w)
	}
	ctx, stop := signal.NotifyContext(context.Background(), syscall.SIGINT, syscall.SIGTERM)
	defer stop()
	if err := app.Run(ctx, cfg, logger, app.ResolveVersion(version), app.RunOptions{}); err != nil {
		logger.Error("server stopped", "err", err.Error())
		var cerr *config.Error
		if errors.As(err, &cerr) {
			return 2
		}
		return 1
	}
	return 0
}

func newLogger(cfg *config.Config, w io.Writer) *slog.Logger {
	var level slog.Level
	_ = level.UnmarshalText([]byte(cfg.LogLevel))
	opts := &slog.HandlerOptions{Level: level}
	if cfg.LogFormat == "text" {
		return slog.New(slog.NewTextHandler(w, opts))
	}
	return slog.New(slog.NewJSONHandler(w, opts))
}
