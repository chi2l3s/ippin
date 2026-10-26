package config

import (
	"encoding/json"
	"os"
	"path/filepath"
	"slices"
	"strings"
)

type Config struct {
	LockedIP      string   `json:"locked_ip"`
	LockedCountry string   `json:"locked_country"`
	Port          int      `json:"port"`
	UpstreamProxy string   `json:"upstream_proxy"`
	AutoWinPAC    bool     `json:"auto_win_pac"`
	Domains       []string `json:"domains"`
}

func DefaultPresets() map[string][]string {
	return map[string][]string{
		"claude": {
			"claude.ai",
			"anthropic.com",
			"claude.com",
		},
		"openai": {
			"chatgpt.com",
			"openai.com",
			"oaistatic.com",
			"oaiusercontent.com",
		},
		"gemini": {
			"gemini.google.com",
			"aistudio.google.com",
			"generativelanguage.googleapis.com",
		},
	}
}

func BaseDir() (string, error) {
	home, err := os.UserHomeDir()
	if err != nil {
		return "", err
	}
	dir := filepath.Join(home, ".ippin")
	if err := os.MkdirAll(dir, 0755); err != nil {
		return "", err
	}
	return dir, nil
}

func configPath() (string, error) {
	dir, err := BaseDir()
	if err != nil {
		return "", err
	}
	return filepath.Join(dir, "config.json"), nil
}

func LogPath() string {
	dir, _ := BaseDir()
	return filepath.Join(dir, "ippin.log")
}

func PidPath() string {
	dir, _ := BaseDir()
	return filepath.Join(dir, "ippin.pid")
}

func VbsPath() string {
	dir, _ := BaseDir()
	return filepath.Join(dir, "autostart.vbs")
}

func Load() (*Config, error) {
	path, err := configPath()
	if err != nil {
		return nil, err
	}

	if _, err := os.Stat(path); os.IsNotExist(err) {
		cfg := &Config{
			Port:          8888,
			UpstreamProxy: "auto",
			AutoWinPAC:    true,
			Domains:       DefaultPresets()["claude"],
		}
		_ = cfg.Save()
		return cfg, nil
	}

	data, err := os.ReadFile(path)
	if err != nil {
		return nil, err
	}

	var cfg Config
	if err := json.Unmarshal(data, &cfg); err != nil {
		return nil, err
	}
	if cfg.Port == 0 {
		cfg.Port = 8888
	}
	if cfg.UpstreamProxy == "" {
		cfg.UpstreamProxy = "tun"
	}
	return &cfg, nil
}

func (c *Config) Save() error {
	path, err := configPath()
	if err != nil {
		return err
	}
	data, err := json.MarshalIndent(c, "", "  ")
	if err != nil {
		return err
	}
	return os.WriteFile(path, data, 0644)
}

func (c *Config) AddDomain(domain string) bool {
	domain = strings.ToLower(strings.TrimSpace(domain))
	domain = strings.TrimPrefix(domain, "*.")
	if domain == "" || slices.Contains(c.Domains, domain) {
		return false
	}
	c.Domains = append(c.Domains, domain)
	return true
}
