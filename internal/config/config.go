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

func configPath() (string, error) {
	home, err := os.UserHomeDir()
	if err != nil {
		return "", err
	}
	dir := filepath.Join(home, ".ippin")
	if err := os.MkdirAll(dir, 0755); err != nil {
		return "", err
	}
	return filepath.Join(dir, "config.json"), nil
}

func Load() (*Config, error) {
	path, err := configPath()
	if err != nil {
		return nil, err
	}

	if _, err := os.Stat(path); os.IsNotExist(err) {
		cfg := &Config{
			Port:    8888,
			Domains: DefaultPresets()["claude"],
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
