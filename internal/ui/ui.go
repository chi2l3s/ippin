package ui

import (
	"fmt"
	"os"
	"time"

	"github.com/fatih/color"
	"github.com/chi2l3s/ippin/internal/config"
)

var (
	Cyan    = color.New(color.FgCyan, color.Bold).SprintFunc()
	Green   = color.New(color.FgGreen, color.Bold).SprintFunc()
	Red     = color.New(color.FgRed, color.Bold).SprintFunc()
	Yellow  = color.New(color.FgYellow, color.Bold).SprintFunc()
	White   = color.New(color.FgHiWhite, color.Bold).SprintFunc()
	Gray    = color.New(color.FgHiBlack).SprintFunc()
	BgGreen = color.New(color.BgGreen, color.FgBlack, color.Bold).SprintFunc()
	BgRed   = color.New(color.BgRed, color.FgWhite, color.Bold).SprintFunc()
	BgCyan  = color.New(color.BgCyan, color.FgBlack, color.Bold).SprintFunc()
)

func writeFileLog(line string) {
	f, err := os.OpenFile(config.LogPath(), os.O_CREATE|os.O_WRONLY|os.O_APPEND, 0644)
	if err != nil {
		return
	}
	defer f.Close()
	ts := time.Now().Format("2006-01-02 15:04:05")
	_, _ = fmt.Fprintf(f, "[%s] %s\n", ts, line)
}

func Banner() {
	fmt.Println()
	fmt.Printf("  %s  %s\n", BgCyan(" IPPIN "), White("Zero-Leak IP Gatekeeper & Proxy Guard v1.1"))
	fmt.Println(Gray("  ─────────────────────────────────────────────────────"))
}

func Success(msg string, args ...any) {
	fmt.Printf("  %s %s\n", Green("✓"), fmt.Sprintf(msg, args...))
}

func Error(msg string, args ...any) {
	fmt.Printf("  %s %s\n", Red("✗"), fmt.Sprintf(msg, args...))
}

func Info(msg string, args ...any) {
	fmt.Printf("  %s %s\n", Cyan("ℹ"), fmt.Sprintf(msg, args...))
}

func Warn(msg string, args ...any) {
	fmt.Printf("  %s %s\n", Yellow("⚠"), fmt.Sprintf(msg, args...))
}

func LogPass(host, ip, country string) {
	ts := Gray(time.Now().Format("15:04:05"))
	fmt.Printf("  %s %s %-28s %s %s (%s)\n", ts, BgGreen(" PASS "), White(host), Gray("via"), Green(ip), Cyan(country))
	writeFileLog(fmt.Sprintf("PASS  %-28s via %s (%s)", host, ip, country))
}

func LogBlock(host, currentIP, lockedIP string) {
	ts := Gray(time.Now().Format("15:04:05"))
	fmt.Printf("  %s %s %-28s %s %s %s %s\n", ts, BgRed(" BLOCK "), White(host), Red("IP Mismatch:"), Yellow(currentIP), Gray("!="), Green(lockedIP))
	writeFileLog(fmt.Sprintf("BLOCK %-28s IP Mismatch: %s != %s", host, currentIP, lockedIP))
}

func LogKill(count int, currentIP string) {
	ts := Gray(time.Now().Format("15:04:05"))
	fmt.Printf("  %s %s Dropped %d active session(s)! Detected IP change -> %s\n", ts, BgRed(" KILL "), count, Red(currentIP))
	writeFileLog(fmt.Sprintf("KILL  Dropped %d active session(s)! Detected IP change -> %s", count, currentIP))
}
