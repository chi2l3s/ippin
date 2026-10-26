package cmd

import (
	"strings"

	"github.com/spf13/cobra"
	"github.com/chi2l3s/ippin/internal/config"
	"github.com/chi2l3s/ippin/internal/ui"
	"github.com/chi2l3s/ippin/internal/upstream"
)

var modeCmd = &cobra.Command{
	Use:   "mode [tun|auto|http://127.0.0.1:2080|socks5://127.0.0.1:10808]",
	Short: "Set outbound mode: 'tun', 'auto', or a specific local proxy URL",
	Args:  cobra.ExactArgs(1),
	Run: func(cmd *cobra.Command, args []string) {
		ui.Banner()
		cfg, err := config.Load()
		if err != nil {
			ui.Error("Failed to load config: %v", err)
			return
		}

		arg := strings.TrimSpace(args[0])
		cfg.UpstreamProxy = arg
		if err := cfg.Save(); err != nil {
			ui.Error("Failed to save config: %v", err)
			return
		}

		resolved := upstream.Resolve(cfg.UpstreamProxy, cfg.Port)
		if resolved == "" {
			ui.Success("Mode set to: %s (Direct / TUN Interface)", ui.Cyan(arg))
		} else {
			ui.Success("Mode set to: %s -> Active upstream: %s", ui.Cyan(arg), ui.Green(resolved))
		}
	},
}

func init() {
	rootCmd.AddCommand(modeCmd)
}
