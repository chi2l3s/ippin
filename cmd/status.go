package cmd

import (
	"fmt"
	"strings"
	"time"

	"github.com/chi2l3s/ippin/internal/checker"
	"github.com/chi2l3s/ippin/internal/config"
	"github.com/chi2l3s/ippin/internal/ui"
	"github.com/spf13/cobra"
)

var statusCmd = &cobra.Command{
	Use:   "status",
	Short: "Check current IP status against your pinned IP",
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

		chk := checker.New(cfg.UpstreamProxy, time.Second)
		ok, res := chk.Verify(cfg.LockedIP, true)

		fmt.Printf("  %-18s %s (%s)\n", ui.Gray("Pinned IP:"), ui.Green(cfg.LockedIP), ui.Cyan(cfg.LockedCountry))
		if ok {
			fmt.Printf("  %-18s %s (%s)\n", ui.Gray("Current IP:"), ui.Green(res.IP), ui.Cyan(res.Country))
			fmt.Printf("  %-18s %s\n", ui.Gray("Gate Status:"), ui.BgGreen(" SAFE TO CONNECT "))
		} else {
			fmt.Printf("  %-18s %s (%s)\n", ui.Gray("Current IP:"), ui.Red(res.IP), ui.Yellow(res.Country))
			fmt.Printf("  %-18s %s\n", ui.Gray("Gate Status:"), ui.BgRed(" BLOCKED - IP MISMATCH "))
		}

		fmt.Printf("  %-18s 127.0.0.1:%d\n", ui.Gray("Local Proxy:"), cfg.Port)
		fmt.Printf("  %-18s %s\n", ui.Gray("Protected:"), ui.White(strings.Join(cfg.Domains, ", ")))
		fmt.Println()
	},
}

func init() {
	rootCmd.AddCommand(statusCmd)
}
