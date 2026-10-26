package proxy

import (
	"context"
	"fmt"
	"io"
	"net"
	"net/http"
	"net/http/httputil"
	"net/url"
	"strings"
	"sync"
	"time"

	"github.com/chi2l3s/ippin/internal/checker"
	"github.com/chi2l3s/ippin/internal/config"
	"github.com/chi2l3s/ippin/internal/sysproxy"
	"github.com/chi2l3s/ippin/internal/ui"
	"github.com/chi2l3s/ippin/internal/upstream"
)

type Server struct {
	cfg         *config.Config
	checker     *checker.Checker
	mu          sync.Mutex
	activeConns map[net.Conn]struct{}
	keepWinPAC  bool
}

func NewServer(cfg *config.Config, keepWinPAC bool) *Server {
	return &Server{
		cfg:         cfg,
		checker:     checker.New(cfg.UpstreamProxy, cfg.Port, 3*time.Second),
		activeConns: make(map[net.Conn]struct{}),
		keepWinPAC:  keepWinPAC,
	}
}

func (s *Server) Start(ctx context.Context) error {
	go s.watchNetwork(ctx)

	addr := fmt.Sprintf("127.0.0.1:%d", s.cfg.Port)
	srv := &http.Server{
		Addr:    addr,
		Handler: s,
	}

	go func() {
		<-ctx.Done()
		s.killAllConnections("SHUTDOWN")
		_ = srv.Shutdown(context.Background())
	}()

	return srv.ListenAndServe()
}

func (s *Server) ServeHTTP(w http.ResponseWriter, r *http.Request) {
	if r.Method == http.MethodConnect {
		s.handleConnect(w, r)
		return
	}

	if r.URL.Path == "/proxy.pac" {
		s.handlePAC(w)
		return
	}

	if strings.HasPrefix(r.URL.Path, "/anthropic/") {
		s.handleAnthropicReverseProxy(w, r)
		return
	}

	if r.URL.Host != "" {
		s.handleHTTPForward(w, r)
		return
	}

	w.Header().Set("Content-Type", "application/json")
	w.WriteHeader(http.StatusOK)
	_, _ = fmt.Fprintf(w, `{"status":"active","locked_ip":"%s"}`, s.cfg.LockedIP)
}

func (s *Server) handlePAC(w http.ResponseWriter) {
	w.Header().Set("Content-Type", "application/x-ns-proxy-autoconfig")
	var conds []string
	for _, d := range s.cfg.Domains {
		item := fmt.Sprintf(`dnsDomainIs(host, "%s") || shExpMatch(host, "*.%s")`, d, d)
		conds = append(conds, item)
	}
	rule := strings.Join(conds, " || ")
	if rule == "" {
		rule = "false"
	}

	fallback := upstream.PACFallback(s.cfg.UpstreamProxy, s.cfg.Port)

	pac := fmt.Sprintf(`function FindProxyForURL(url, host) {
    if (%s) {
        return "PROXY 127.0.0.1:%d";
    }
    return "%s";
}`, rule, s.cfg.Port, fallback)

	_, _ = w.Write([]byte(pac))
}

func (s *Server) handleAnthropicReverseProxy(w http.ResponseWriter, r *http.Request) {
	ok, res := s.checker.Verify(s.cfg.LockedIP, false)
	if !ok {
		ui.LogBlock("api.anthropic.com", res.IP, s.cfg.LockedIP)
		http.Error(w, "Blocked by IPPin: External IP mismatch", http.StatusForbidden)
		return
	}

	ui.LogPass("api.anthropic.com", res.IP, res.Country)

	target, _ := url.Parse("https://api.anthropic.com")
	proxy := httputil.NewSingleHostReverseProxy(target)
	proxy.Transport = &http.Transport{
		DialContext: func(ctx context.Context, network, addr string) (net.Conn, error) {
			return upstream.Dial(s.cfg.UpstreamProxy, s.cfg.Port, addr, 10*time.Second)
		},
	}

	r.URL.Path = strings.TrimPrefix(r.URL.Path, "/anthropic")
	r.Host = target.Host
	proxy.ServeHTTP(w, r)
}

func (s *Server) handleHTTPForward(w http.ResponseWriter, r *http.Request) {
	ok, res := s.checker.Verify(s.cfg.LockedIP, false)
	if !ok {
		ui.LogBlock(r.Host, res.IP, s.cfg.LockedIP)
		http.Error(w, "Blocked by IPPin: External IP mismatch", http.StatusForbidden)
		return
	}

	ui.LogPass(r.Host, res.IP, res.Country)

	transport := &http.Transport{
		DialContext: func(ctx context.Context, network, addr string) (net.Conn, error) {
			return upstream.Dial(s.cfg.UpstreamProxy, s.cfg.Port, addr, 10*time.Second)
		},
	}

	r.RequestURI = ""
	resp, err := transport.RoundTrip(r)
	if err != nil {
		http.Error(w, err.Error(), http.StatusBadGateway)
		return
	}
	defer resp.Body.Close()

	for k, vv := range resp.Header {
		for _, v := range vv {
			w.Header().Add(k, v)
		}
	}
	w.WriteHeader(resp.StatusCode)
	_, _ = io.Copy(w, resp.Body)
}

func (s *Server) handleConnect(w http.ResponseWriter, r *http.Request) {
	ok, res := s.checker.Verify(s.cfg.LockedIP, false)
	if !ok {
		ui.LogBlock(r.Host, res.IP, s.cfg.LockedIP)
		http.Error(w, "Blocked by IPPin: External IP mismatch", http.StatusForbidden)
		return
	}

	ui.LogPass(r.Host, res.IP, res.Country)

	targetConn, err := upstream.Dial(s.cfg.UpstreamProxy, s.cfg.Port, r.Host, 10*time.Second)
	if err != nil {
		http.Error(w, err.Error(), http.StatusServiceUnavailable)
		return
	}

	hijacker, ok := w.(http.Hijacker)
	if !ok {
		targetConn.Close()
		http.Error(w, "Hijacking not supported", http.StatusInternalServerError)
		return
	}

	clientConn, _, err := hijacker.Hijack()
	if err != nil {
		targetConn.Close()
		return
	}

	_, _ = clientConn.Write([]byte("HTTP/1.1 200 Connection Established\r\n\r\n"))

	s.trackConn(clientConn, true)
	s.trackConn(targetConn, true)

	go func() {
		_, _ = io.Copy(targetConn, clientConn)
		s.trackConn(clientConn, false)
		s.trackConn(targetConn, false)
		clientConn.Close()
		targetConn.Close()
	}()

	go func() {
		_, _ = io.Copy(clientConn, targetConn)
		s.trackConn(clientConn, false)
		s.trackConn(targetConn, false)
		clientConn.Close()
		targetConn.Close()
	}()
}

func (s *Server) trackConn(c net.Conn, add bool) {
	s.mu.Lock()
	defer s.mu.Unlock()
	if add {
		s.activeConns[c] = struct{}{}
	} else {
		delete(s.activeConns, c)
	}
}

func (s *Server) activeCount() int {
	s.mu.Lock()
	defer s.mu.Unlock()
	return len(s.activeConns)
}

func (s *Server) killAllConnections(currentIP string) {
	s.mu.Lock()
	count := len(s.activeConns)
	for c := range s.activeConns {
		_ = c.Close()
		delete(s.activeConns, c)
	}
	s.mu.Unlock()

	if count > 0 && currentIP != "SHUTDOWN" {
		ui.LogKill(count/2, currentIP)
	}
}

func (s *Server) watchNetwork(ctx context.Context) {
	ticker := time.NewTicker(2 * time.Second)
	defer ticker.Stop()

	ticks := 0
	for {
		select {
		case <-ctx.Done():
			return
		case <-ticker.C:
			ticks++
			if s.keepWinPAC && ticks%5 == 0 {
				_ = sysproxy.EnableWindowsPAC(s.cfg.Port)
			}
			if s.activeCount() == 0 {
				continue
			}
			ok, res := s.checker.Verify(s.cfg.LockedIP, true)
			if !ok {
				s.killAllConnections(res.IP)
			}
		}
	}
}
