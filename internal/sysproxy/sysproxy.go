package sysproxy

import (
	"errors"
	"fmt"
	"os"
	"os/exec"
	"runtime"
	"strconv"
	"strings"

	"github.com/chi2l3s/ippin/internal/config"
)

const (
	inetRegPath = `HKCU\Software\Microsoft\Windows\CurrentVersion\Internet Settings`
	runRegPath  = `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`
	appName     = "IPPinGatekeeper"
)

func EnableWindowsPAC(port int) error {
	if runtime.GOOS != "windows" {
		return errors.New("automatic system PAC configuration is only supported on Windows")
	}
	pacURL := fmt.Sprintf("http://127.0.0.1:%d/proxy.pac", port)
	_ = exec.Command("reg", "add", inetRegPath, "/v", "ProxyEnable", "/t", "REG_DWORD", "/d", "0", "/f").Run()
	return exec.Command("reg", "add", inetRegPath, "/v", "AutoConfigURL", "/t", "REG_SZ", "/d", pacURL, "/f").Run()
}

func DisableWindowsPAC() error {
	if runtime.GOOS != "windows" {
		return errors.New("automatic system PAC configuration is only supported on Windows")
	}
	return exec.Command("reg", "delete", inetRegPath, "/v", "AutoConfigURL", "/f").Run()
}

func InstallAutostart() error {
	if runtime.GOOS != "windows" {
		return errors.New("Windows autostart is only supported on Windows")
	}
	exePath, err := os.Executable()
	if err != nil {
		return err
	}

	vbsContent := fmt.Sprintf(`CreateObject("WScript.Shell").Run """%s"" start --headless -w", 0, False`, exePath)
	vbsPath := config.VbsPath()
	if err := os.WriteFile(vbsPath, []byte(vbsContent), 0644); err != nil {
		return err
	}

	cmdVal := fmt.Sprintf(`wscript.exe "%s"`, vbsPath)
	return exec.Command("reg", "add", runRegPath, "/v", appName, "/t", "REG_SZ", "/d", cmdVal, "/f").Run()
}

func UninstallAutostart() error {
	if runtime.GOOS != "windows" {
		return errors.New("Windows autostart is only supported on Windows")
	}
	_ = os.Remove(config.VbsPath())
	_ = exec.Command("reg", "delete", runRegPath, "/v", appName, "/f").Run()
	return nil
}

func StartDaemonProcess() (int, error) {
	if pid, running := GetDaemonPID(); running {
		return pid, nil
	}

	exePath, err := os.Executable()
	if err != nil {
		return 0, err
	}

	if runtime.GOOS == "windows" {
		vbsContent := fmt.Sprintf(`CreateObject("WScript.Shell").Run """%s"" start --headless -w", 0, False`, exePath)
		vbsPath := config.VbsPath()
		if err := os.WriteFile(vbsPath, []byte(vbsContent), 0644); err != nil {
			return 0, err
		}
		if err := exec.Command("wscript.exe", vbsPath).Start(); err != nil {
			return 0, err
		}
		return 0, nil
	}

	cmd := exec.Command(exePath, "start", "--headless")
	if err := cmd.Start(); err != nil {
		return 0, err
	}
	return cmd.Process.Pid, nil
}

func StopDaemonProcess() error {
	pid, running := GetDaemonPID()
	if !running {
		_ = DisableWindowsPAC()
		return nil
	}

	if runtime.GOOS == "windows" {
		_ = exec.Command("taskkill", "/F", "/PID", strconv.Itoa(pid)).Run()
	} else {
		if p, err := os.FindProcess(pid); err == nil {
			_ = p.Kill()
		}
	}

	_ = os.Remove(config.PidPath())
	_ = DisableWindowsPAC()
	return nil
}

func GetDaemonPID() (int, bool) {
	data, err := os.ReadFile(config.PidPath())
	if err != nil {
		return 0, false
	}
	pid, err := strconv.Atoi(strings.TrimSpace(string(data)))
	if err != nil || pid <= 0 {
		return 0, false
	}

	if runtime.GOOS == "windows" {
		out, err := exec.Command("tasklist", "/FI", fmt.Sprintf("PID eq %d", pid), "/NH").Output()
		if err != nil {
			return 0, false
		}
		if strings.Contains(string(out), strconv.Itoa(pid)) {
			return pid, true
		}
		return 0, false
	}

	p, err := os.FindProcess(pid)
	if err != nil {
		return 0, false
	}
	return p.Pid, true
}
