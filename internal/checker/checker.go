package checker

import (
	"io"
	"net/http"
	"strings"
	"sync"
	"time"

	"github.com/chi2l3s/ippin/internal/upstream"
)

type TraceResult struct {
	IP      string
	Country string
}

type Checker struct {
	mu            sync.Mutex
	cached        *TraceResult
	lastCheck     time.Time
	cacheTTL      time.Duration
	upstreamProxy string
	ownPort       int
}

func New(upstreamProxy string, ownPort int, cacheTTL time.Duration) *Checker {
	return &Checker{
		upstreamProxy: upstreamProxy,
		ownPort:       ownPort,
		cacheTTL:      cacheTTL,
	}
}

func (c *Checker) FetchCurrent(force bool) (*TraceResult, error) {
	c.mu.Lock()
	defer c.mu.Unlock()

	if !force && c.cached != nil && time.Since(c.lastCheck) < c.cacheTTL {
		return c.cached, nil
	}

	client := upstream.HTTPClient(c.upstreamProxy, c.ownPort, 4*time.Second)

	endpoints := []string{
		"https://1.1.1.1/cdn-cgi/trace",
		"https://1.0.0.1/cdn-cgi/trace",
	}

	var lastErr error
	for _, ep := range endpoints {
		req, err := http.NewRequest(http.MethodGet, ep, nil)
		if err != nil {
			lastErr = err
			continue
		}
		req.Header.Set("User-Agent", "ippin-gatekeeper/1.1")

		resp, err := client.Do(req)
		if err != nil {
			lastErr = err
			continue
		}

		body, err := io.ReadAll(resp.Body)
		resp.Body.Close()
		if err != nil {
			lastErr = err
			continue
		}

		res := parseTrace(string(body))
		if res.IP != "" {
			c.cached = res
			c.lastCheck = time.Now()
			return res, nil
		}
	}

	return nil, lastErr
}

func (c *Checker) Verify(lockedIP string, force bool) (bool, *TraceResult) {
	res, err := c.FetchCurrent(force)
	if err != nil || res == nil {
		return false, &TraceResult{IP: "UNREACHABLE", Country: "??"}
	}
	return res.IP == lockedIP, res
}

func parseTrace(raw string) *TraceResult {
	res := &TraceResult{Country: "UNKNOWN"}
	lines := strings.Split(raw, "\n")
	for _, line := range lines {
		parts := strings.SplitN(strings.TrimSpace(line), "=", 2)
		if len(parts) != 2 {
			continue
		}
		switch parts[0] {
		case "ip":
			res.IP = parts[1]
		case "loc":
			res.Country = parts[1]
		}
	}
	return res
}
