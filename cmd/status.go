package cmd

import (
	"fmt"
	"strings"
	"time"

	"github.com/spf13/cobra"
	"github.com/chi2l3s/ippin/internal/checker"
	"github.com/chi2l3s/ippin/internal/config"
	"github.com/chi2l3s/ippin/internal/sysproxy"
	"github.com/chi2l3s/ippin/internal/ui"
	"github.com/chi2l3s/ippin/internal/upstream"
)

var statusCmd = &cobra.Command{
	Use:   "status",
	Short: "Check current IP status, daemon state, and active proxy mode",
	Run: func(cmd *cobra.Command, args []string) {
		ui.Banner()
		cfg, err := config.Load()
		if err != nil {
			ui.Error("Failed to load config: %v", err)
			return
		}

		if cfg.LockedIP == "" {
			ui.Warn("No IP is locked yet! Run 'ippin lock' first.")
			return
		}

		if pid, running := sysproxy.GetDaemonPID(); running {
			fmt.Printf("  %-18s %s (PID: %d)\n", ui.Gray("Daemon Service:"), ui.BgGreen(" RUNNING "), pid)
		} else {
			fmt.Printf("  %-18s %s\n", ui.Gray("Daemon Service:"), ui.Yellow("Not running in background"))
		}

		resolved := upstream.Resolve(cfg.UpstreamProxy, cfg.Port)
		if resolved == "" {
			fmt.Printf("  %-18s %s (mode: %s)\n", ui.Gray("Outbound Route:"), ui.Cyan("TUN / Direct"), cfg.UpstreamProxy)
		} else {
			fmt.Printf("  %-18s %s (mode: %s)\n", ui.Gray("Outbound Route:"), ui.Cyan(resolved), cfg.UpstreamProxy)
		}

		chk := checker.New(cfg.UpstreamProxy, cfg.Port, time.Second)
		ok, res := chk.Verify(cfg.LockedIP, true)

		fmt.Printf("  %-18s %s (%s)\n", ui.Gray("Pinned IP:"), ui.Green(cfg.LockedIP), ui.Cyan(cfg.LockedCountry))
		if ok {
			fmt.Printf("  %-18s %s (%s)\n", ui.Gray("Current IP:"), ui.Green(res.IP), ui.Cyan(res.Country))
			fmt.Printf("  %-18s %s\n", ui.Gray("Gate Status:"), ui.BgGreen(" SAFE TO CONNECT "))
		} else {
			fmt.Printf("  %-18s %s (%s)\n", ui.Gray("Current IP:"), ui.Red(res.IP), ui.Yellow(res.Country))
			fmt.Printf("  %-18s %s\n", ui.Gray("Gate Status:"), ui.BgRed(" BLOCKED - IP MISMATCH "))
		}

		fmt.Printf("  %-18s 127.0.0.1:%d\n", ui.Gray("Local Gate:"), cfg.Port)
		fmt.Printf("  %-18s %s\n", ui.Gray("Protected:"), ui.White(strings.Join(cfg.Domains, ", ")))
		fmt.Println()
	},
}

func init() {
	rootCmd.AddCommand(statusCmd)
}
