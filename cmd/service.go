package cmd

import (
	"fmt"
	"os"
	"strings"
	"time"

	"github.com/spf13/cobra"
	"github.com/chi2l3s/ippin/internal/config"
	"github.com/chi2l3s/ippin/internal/sysproxy"
	"github.com/chi2l3s/ippin/internal/ui"
)

var linesFlag int

var serviceCmd = &cobra.Command{
	Use:   "service [install|uninstall|start|stop|status]",
	Short: "Manage IPPin background daemon and Windows autostart",
	Args:  cobra.ExactArgs(1),
	Run: func(cmd *cobra.Command, args []string) {
		ui.Banner()
		cfg, err := config.Load()
		if err != nil {
			ui.Error("Failed to load config: %v", err)
			return
		}

		if cfg.LockedIP == "" && (args[0] == "install" || args[0] == "start") {
			ui.Error("No IP locked! Turn on your VPN and run 'ippin lock' first.")
			return
		}

		switch args[0] {
		case "install":
			if err := sysproxy.InstallAutostart(); err != nil {
				ui.Error("Failed to install autostart: %v", err)
				return
			}
			_, err := sysproxy.StartDaemonProcess()
			if err != nil {
				ui.Error("Failed to start background service: %v", err)
				return
			}
			time.Sleep(600 * time.Millisecond)
			pid, _ := sysproxy.GetDaemonPID()
			ui.Success("Service installed to Windows Autostart!")
			ui.Success("Background daemon started (PID: %d) on 127.0.0.1:%d", pid, cfg.Port)

		case "uninstall":
			_ = sysproxy.StopDaemonProcess()
			if err := sysproxy.UninstallAutostart(); err != nil {
				ui.Error("Failed to remove autostart: %v", err)
				return
			}
			ui.Success("Background service stopped and removed from Windows Autostart.")

		case "start":
			_, err := sysproxy.StartDaemonProcess()
			if err != nil {
				ui.Error("Failed to start service: %v", err)
				return
			}
			time.Sleep(600 * time.Millisecond)
			pid, _ := sysproxy.GetDaemonPID()
			ui.Success("Background daemon started (PID: %d) on 127.0.0.1:%d", pid, cfg.Port)

		case "stop":
			if err := sysproxy.StopDaemonProcess(); err != nil {
				ui.Error("Failed to stop service: %v", err)
				return
			}
			ui.Success("Background daemon stopped and Windows PAC restored.")

		case "status":
			if pid, running := sysproxy.GetDaemonPID(); running {
				ui.Success("Service Status: %s (PID: %d)", ui.BgGreen(" RUNNING "), pid)
			} else {
				ui.Warn("Service Status: %s", ui.BgRed(" STOPPED "))
			}

		default:
			ui.Error("Unknown action. Use: install, uninstall, start, stop, status")
		}
	},
}

var logsCmd = &cobra.Command{
	Use:   "logs",
	Short: "Show recent logs from the background service",
	Run: func(cmd *cobra.Command, args []string) {
		ui.Banner()
		data, err := os.ReadFile(config.LogPath())
		if err != nil {
			ui.Info("No logs found yet (%s)", config.LogPath())
			return
		}
		lines := strings.Split(strings.TrimSpace(string(data)), "\n")
		start := 0
		if len(lines) > linesFlag {
			start = len(lines) - linesFlag
		}
		for _, line := range lines[start:] {
			fmt.Printf("  %s\n", line)
		}
		fmt.Println()
	},
}

func init() {
	logsCmd.Flags().IntVarP(&linesFlag, "lines", "n", 30, "Number of log lines to display")
	rootCmd.AddCommand(serviceCmd)
	rootCmd.AddCommand(logsCmd)
}
