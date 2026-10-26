package cmd

import (
	"context"
	"fmt"
	"os"
	"os/signal"
	"syscall"

	"github.com/chi2l3s/ippin/internal/config"
	"github.com/chi2l3s/ippin/internal/proxy"
	"github.com/chi2l3s/ippin/internal/sysproxy"
	"github.com/chi2l3s/ippin/internal/ui"
	"github.com/spf13/cobra"
)

var (
	portFlag    int
	autoWinFlag bool
)

var startCmd = &cobra.Command{
	Use:   "start",
	Short: "Start the local IP gatekeeper proxy server",
	Run: func(cmd *cobra.Command, args []string) {
		ui.Banner()
		cfg, err := config.Load()
		if err != nil {
			ui.Error("Failed to load config: %v", err)
			return
		}

		if cfg.LockedIP == "" {
			ui.Error("No IP locked! Turn on your VPN and run 'ippin lock' first.")
			return
		}

		if portFlag != 0 {
			cfg.Port = portFlag
			_ = cfg.Save()
		}

		if autoWinFlag {
			if err := sysproxy.EnableWindowsPAC(cfg.Port); err != nil {
				ui.Warn("Could not auto-set Windows PAC: %v", err)
			} else {
				ui.Success("Windows System PAC auto-configured!")
				defer func() {
					_ = sysproxy.DisableWindowsPAC()
					ui.Info("Windows System PAC restored.")
				}()
			}
		}

		ui.Info("Pinned IP:       %s (%s)", ui.Green(cfg.LockedIP), ui.Cyan(cfg.LockedCountry))
		ui.Info("HTTP/HTTPS Gate: %s", ui.White(fmt.Sprintf("http://127.0.0.1:%d", cfg.Port)))
		ui.Info("PAC Auto-Script: %s", ui.Cyan(fmt.Sprintf("http://127.0.0.1:%d/proxy.pac", cfg.Port)))
		ui.Info("Anthropic API:   %s", ui.Yellow(fmt.Sprintf("http://127.0.0.1:%d/anthropic", cfg.Port)))
		fmt.Println(ui.Gray("  ─────────────────────────────────────────────────────"))

		ctx, stop := signal.NotifyContext(context.Background(), os.Interrupt, syscall.SIGTERM)
		defer stop()

		srv := proxy.NewServer(cfg)
		if err := srv.Start(ctx); err != nil && err.Error() != "http: Server closed" {
			ui.Error("Server error: %v", err)
		}
	},
}

func init() {
	startCmd.Flags().IntVarP(&portFlag, "port", "p", 0, "Local proxy port (default 8888)")
	startCmd.Flags().BoolVarP(&autoWinFlag, "win-pac", "w", false, "Automatically enable Windows PAC proxy while running")
	rootCmd.AddCommand(startCmd)
}
