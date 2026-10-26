package cmd

import (
	"github.com/chi2l3s/ippin/internal/config"
	"github.com/chi2l3s/ippin/internal/sysproxy"
	"github.com/chi2l3s/ippin/internal/ui"
	"github.com/spf13/cobra"
)

var winCmd = &cobra.Command{
	Use:   "win [enable|disable]",
	Short: "Enable or disable Windows automatic PAC routing in 1 click",
	Args:  cobra.ExactArgs(1),
	Run: func(cmd *cobra.Command, args []string) {
		ui.Banner()
		cfg, err := config.Load()
		if err != nil {
			ui.Error("Failed to load config: %v", err)
			return
		}

		switch args[0] {
		case "enable":
			if err := sysproxy.EnableWindowsPAC(cfg.Port); err != nil {
				ui.Error("Failed to set Windows PAC: %v", err)
				return
			}
			ui.Success("Windows PAC enabled -> http://127.0.0.1:%d/proxy.pac", cfg.Port)
		case "disable":
			if err := sysproxy.DisableWindowsPAC(); err != nil {
				ui.Error("Failed to remove Windows PAC: %v", err)
				return
			}
			ui.Success("Windows PAC disabled.")
		default:
			ui.Error("Use 'ippin win enable' or 'ippin win disable'")
		}
	},
}

func init() {
	rootCmd.AddCommand(winCmd)
}
