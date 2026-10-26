package cmd

import (
	"time"

	"github.com/spf13/cobra"
	"github.com/chi2l3s/ippin/internal/checker"
	"github.com/chi2l3s/ippin/internal/config"
	"github.com/chi2l3s/ippin/internal/ui"
	"github.com/chi2l3s/ippin/internal/upstream"
)

var upstreamFlag string

var lockCmd = &cobra.Command{
	Use:   "lock",
	Short: "Pin your current external IP address as the only allowed IP",
	Run: func(cmd *cobra.Command, args []string) {
		ui.Banner()
		cfg, err := config.Load()
		if err != nil {
			ui.Error("Failed to load config: %v", err)
			return
		}

		if upstreamFlag != "" {
			cfg.UpstreamProxy = upstreamFlag
		}

		resolved := upstream.Resolve(cfg.UpstreamProxy, cfg.Port)
		if resolved != "" {
			ui.Info("Using detected upstream proxy: %s", ui.Cyan(resolved))
		} else {
			ui.Info("Using TUN / Direct network route...")
		}

		chk := checker.New(cfg.UpstreamProxy, cfg.Port, time.Second)
		res, err := chk.FetchCurrent(true)
		if err != nil {
			ui.Error("Could not detect external IP: %v", err)
			return
		}

		cfg.LockedIP = res.IP
		cfg.LockedCountry = res.Country
		if err := cfg.Save(); err != nil {
			ui.Error("Failed to save config: %v", err)
			return
		}

		ui.Success("External IP detected: %s (%s)", ui.Green(res.IP), ui.Cyan(res.Country))
		ui.Success("IP successfully pinned! Any requests from other IPs will be blocked.")
	},
}

func init() {
	lockCmd.Flags().StringVarP(&upstreamFlag, "upstream", "u", "", "Set mode: 'tun', 'auto', or proxy URL (e.g. http://127.0.0.1:2080)")
	rootCmd.AddCommand(lockCmd)
}
