package cmd

import (
	"context"
	"fmt"
	"os"
	"os/signal"
	"strconv"
	"syscall"

	"github.com/spf13/cobra"
	"github.com/chi2l3s/ippin/internal/config"
	"github.com/chi2l3s/ippin/internal/proxy"
	"github.com/chi2l3s/ippin/internal/sysproxy"
	"github.com/chi2l3s/ippin/internal/ui"
	"github.com/chi2l3s/ippin/internal/upstream"
)

var (
	portFlag     int
	autoWinFlag  bool
	headlessFlag bool
)

var startCmd = &cobra.Command{
	Use:   "start",
	Short: "Start the local IP gatekeeper proxy server",
	Run: func(cmd *cobra.Command, args []string) {
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

		if headlessFlag {
			_ = os.WriteFile(config.PidPath(), []byte(strconv.Itoa(os.Getpid())), 0644)
			defer func() {
				_ = os.Remove(config.PidPath())
			}()
		} else {
			ui.Banner()
		}

		if autoWinFlag {
			if err := sysproxy.EnableWindowsPAC(cfg.Port); err != nil {
				if !headlessFlag {
					ui.Warn("Could not auto-set Windows PAC: %v", err)
				}
			} else {
				if !headlessFlag {
					ui.Success("Windows System PAC auto-configured!")
				}
				defer func() {
					_ = sysproxy.DisableWindowsPAC()
				}()
			}
		}

		if !headlessFlag {
			resolved := upstream.Resolve(cfg.UpstreamProxy, cfg.Port)
			modeDesc := "TUN / Direct"
			if resolved != "" {
				modeDesc = fmt.Sprintf("Proxy (%s)", resolved)
			}
			ui.Info("Pinned IP:       %s (%s)", ui.Green(cfg.LockedIP), ui.Cyan(cfg.LockedCountry))
			ui.Info("Outbound Mode:   %s (config: %s)", ui.White(modeDesc), cfg.UpstreamProxy)
			ui.Info("HTTP/HTTPS Gate: %s", ui.White(fmt.Sprintf("http://127.0.0.1:%d", cfg.Port)))
			ui.Info("PAC Auto-Script: %s", ui.Cyan(fmt.Sprintf("http://127.0.0.1:%d/proxy.pac", cfg.Port)))
			ui.Info("Anthropic API:   %s", ui.Yellow(fmt.Sprintf("http://127.0.0.1:%d/anthropic", cfg.Port)))
			fmt.Println(ui.Gray("  ─────────────────────────────────────────────────────"))
		}

		ctx, stop := signal.NotifyContext(context.Background(), os.Interrupt, syscall.SIGTERM)
		defer stop()

		srv := proxy.NewServer(cfg, autoWinFlag)
		if err := srv.Start(ctx); err != nil && err.Error() != "http: Server closed" {
			if !headlessFlag {
				ui.Error("Server error: %v", err)
			}
		}
	},
}

func init() {
	startCmd.Flags().IntVarP(&portFlag, "port", "p", 0, "Local proxy port (default 8888)")
	startCmd.Flags().BoolVarP(&autoWinFlag, "win-pac", "w", false, "Automatically enable Windows PAC proxy while running")
	startCmd.Flags().BoolVar(&headlessFlag, "headless", false, "Run in background daemon mode")
	_ = startCmd.Flags().MarkHidden("headless")
	rootCmd.AddCommand(startCmd)
}
