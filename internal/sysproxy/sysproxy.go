package sysproxy

import (
	"errors"
	"fmt"
	"os/exec"
	"runtime"
)

const regPath = `HKCU\Software\Microsoft\Windows\CurrentVersion\Internet Settings`

func EnableWindowsPAC(port int) error {
	if runtime.GOOS != "windows" {
		return errors.New("automatic system PAC configuration is only supported on Windows")
	}
	pacURL := fmt.Sprintf("http://127.0.0.1:%d/proxy.pac", port)
	cmd := exec.Command("reg", "add", regPath, "/v", "AutoConfigURL", "/t", "REG_SZ", "/d", pacURL, "/f")
	return cmd.Run()
}

func DisableWindowsPAC() error {
	if runtime.GOOS != "windows" {
		return errors.New("automatic system PAC configuration is only supported on Windows")
	}
	cmd := exec.Command("reg", "delete", regPath, "/v", "AutoConfigURL", "/f")
	return cmd.Run()
}
