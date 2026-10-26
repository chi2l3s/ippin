package cmd

import (
	"strings"

	"github.com/chi2l3s/ippin/internal/config"
	"github.com/chi2l3s/ippin/internal/ui"
	"github.com/spf13/cobra"
)

var presetFlag string

var addCmd = &cobra.Command{
	Use:   "add [domain...]",
	Short: "Add domains or presets to the protected list",
	Run: func(cmd *cobra.Command, args []string) {
		ui.Banner()
		cfg, err := config.Load()
		if err != nil {
			ui.Error("Failed to load config: %v", err)
			return
		}

		if presetFlag != "" {
			presets := config.DefaultPresets()
			list, ok := presets[strings.ToLower(presetFlag)]
			if !ok {
				ui.Error("Unknown preset '%s'. Available: claude, openai, gemini", presetFlag)
				return
			}
			for _, d := range list {
				cfg.AddDomain(d)
			}
			_ = cfg.Save()
			ui.Success("Added preset '%s': %s", ui.Cyan(presetFlag), strings.Join(list, ", "))
			return
		}

		if len(args) == 0 {
			ui.Warn("Specify a domain (e.g. ippin add claude.ai) or --preset claude")
			return
		}

		for _, d := range args {
			if cfg.AddDomain(d) {
				ui.Success("Added protected domain: %s", ui.White(d))
			} else {
				ui.Info("Domain already present: %s", d)
			}
		}
		_ = cfg.Save()
	},
}

func init() {
	addCmd.Flags().StringVarP(&presetFlag, "preset", "p", "", "Add domain preset (claude, openai, gemini)")
	rootCmd.AddCommand(addCmd)
}
