package upstream

import (
	"bufio"
	"context"
	"errors"
	"fmt"
	"io"
	"net"
	"net/http"
	"net/url"
	"strconv"
	"strings"
	"time"
)

var commonPorts = []struct {
	port   int
	scheme string
}{
	{2080, "http"},
	{12334, "http"},
	{7890, "http"},
	{10809, "http"},
	{10808, "socks5"},
}

func Resolve(configured string, ownPort int) string {
	mode := strings.ToLower(strings.TrimSpace(configured))
	if mode == "" || mode == "tun" || mode == "direct" {
		return ""
	}

	if mode == "auto" {
		for _, cand := range commonPorts {
			if cand.port == ownPort {
				continue
			}
			addr := fmt.Sprintf("127.0.0.1:%d", cand.port)
			conn, err := net.DialTimeout("tcp", addr, 120*time.Millisecond)
			if err == nil {
				_ = conn.Close()
				return fmt.Sprintf("%s://%s", cand.scheme, addr)
			}
		}
		return ""
	}

	if !strings.Contains(configured, "://") {
		return "http://" + configured
	}
	return configured
}

func PACFallback(configured string, ownPort int) string {
	resolved := Resolve(configured, ownPort)
	if resolved == "" {
		return "DIRECT"
	}
	u, err := url.Parse(resolved)
	if err != nil {
		return "DIRECT"
	}
	if u.Scheme == "socks5" || u.Scheme == "socks" {
		return fmt.Sprintf("SOCKS5 %s; SOCKS %s; DIRECT", u.Host, u.Host)
	}
	return fmt.Sprintf("PROXY %s; DIRECT", u.Host)
}

func Dial(configured string, ownPort int, targetAddr string, timeout time.Duration) (net.Conn, error) {
	resolved := Resolve(configured, ownPort)
	if resolved == "" {
		return net.DialTimeout("tcp", targetAddr, timeout)
	}

	u, err := url.Parse(resolved)
	if err != nil {
		return nil, err
	}

	if u.Scheme == "socks5" || u.Scheme == "socks" {
		return dialSOCKS5(u.Host, targetAddr, timeout)
	}
	return dialHTTPConnect(u.Host, targetAddr, timeout)
}

func HTTPClient(configured string, ownPort int, timeout time.Duration) *http.Client {
	transport := &http.Transport{
		DisableKeepAlives: true,
		DialContext: func(ctx context.Context, network, addr string) (net.Conn, error) {
			return Dial(configured, ownPort, addr, timeout)
		},
	}
	return &http.Client{
		Transport: transport,
		Timeout:   timeout,
	}
}

func dialHTTPConnect(proxyHost, targetAddr string, timeout time.Duration) (net.Conn, error) {
	conn, err := net.DialTimeout("tcp", proxyHost, timeout)
	if err != nil {
		return nil, err
	}
	_ = conn.SetDeadline(time.Now().Add(timeout))

	req := fmt.Sprintf("CONNECT %s HTTP/1.1\r\nHost: %s\r\n\r\n", targetAddr, targetAddr)
	if _, err := conn.Write([]byte(req)); err != nil {
		_ = conn.Close()
		return nil, err
	}

	resp, err := http.ReadResponse(bufio.NewReader(conn), nil)
	if err != nil {
		_ = conn.Close()
		return nil, err
	}
	if resp.StatusCode != http.StatusOK {
		_ = conn.Close()
		return nil, fmt.Errorf("upstream proxy status: %d", resp.StatusCode)
	}

	_ = conn.SetDeadline(time.Time{})
	return conn, nil
}

func dialSOCKS5(proxyHost, targetAddr string, timeout time.Duration) (net.Conn, error) {
	conn, err := net.DialTimeout("tcp", proxyHost, timeout)
	if err != nil {
		return nil, err
	}
	_ = conn.SetDeadline(time.Now().Add(timeout))

	if _, err := conn.Write([]byte{0x05, 0x01, 0x00}); err != nil {
		_ = conn.Close()
		return nil, err
	}

	authResp := make([]byte, 2)
	if _, err := io.ReadFull(conn, authResp); err != nil {
		_ = conn.Close()
		return nil, err
	}
	if authResp[0] != 0x05 || authResp[1] != 0x00 {
		_ = conn.Close()
		return nil, errors.New("socks5 authentication failed")
	}

	host, portStr, err := net.SplitHostPort(targetAddr)
	if err != nil {
		_ = conn.Close()
		return nil, err
	}
	port, err := strconv.Atoi(portStr)
	if err != nil {
		_ = conn.Close()
		return nil, err
	}

	req := []byte{0x05, 0x01, 0x00, 0x03, byte(len(host))}
	req = append(req, []byte(host)...)
	req = append(req, byte(port>>8), byte(port&0xff))

	if _, err := conn.Write(req); err != nil {
		_ = conn.Close()
		return nil, err
	}

	hdr := make([]byte, 4)
	if _, err := io.ReadFull(conn, hdr); err != nil {
		_ = conn.Close()
		return nil, err
	}
	if hdr[1] != 0x00 {
		_ = conn.Close()
		return nil, fmt.Errorf("socks5 connect error: %d", hdr[1])
	}

	var skip int
	switch hdr[3] {
	case 0x01:
		skip = 4 + 2
	case 0x03:
		lenBuf := make([]byte, 1)
		if _, err := io.ReadFull(conn, lenBuf); err != nil {
			_ = conn.Close()
			return nil, err
		}
		skip = int(lenBuf[0]) + 2
	case 0x04:
		skip = 16 + 2
	}

	if skip > 0 {
		trash := make([]byte, skip)
		if _, err := io.ReadFull(conn, trash); err != nil {
			_ = conn.Close()
			return nil, err
		}
	}

	_ = conn.SetDeadline(time.Time{})
	return conn, nil
}
