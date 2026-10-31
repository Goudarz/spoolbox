// Testing a sign-up email with Spoolbox from Go (standard library only).
//
//	cd examples/go && go test ./...
//
// Set SPOOLBOX_URL / SPOOLBOX_SMTP if the server does not listen on the defaults.
package signup

import (
	"bytes"
	"encoding/json"
	"fmt"
	"net/http"
	"net/smtp"
	"os"
	"strings"
	"testing"
)

func env(name, fallback string) string {
	if v := os.Getenv(name); v != "" {
		return v
	}
	return fallback
}

var (
	baseURL  = strings.TrimSuffix(env("SPOOLBOX_URL", "http://127.0.0.1:8025/"), "/") + "/"
	smtpAddr = env("SPOOLBOX_SMTP", "127.0.0.1:1025")
)

func api(t *testing.T, method, path string, body, out any) {
	t.Helper()
	var payload bytes.Buffer
	if body != nil {
		if err := json.NewEncoder(&payload).Encode(body); err != nil {
			t.Fatal(err)
		}
	}
	req, err := http.NewRequest(method, baseURL+path, &payload)
	if err != nil {
		t.Fatal(err)
	}
	req.Header.Set("Content-Type", "application/json")
	res, err := http.DefaultClient.Do(req)
	if err != nil {
		t.Fatal(err)
	}
	defer res.Body.Close()
	if res.StatusCode >= 300 {
		t.Fatalf("%s %s: %s", method, path, res.Status)
	}
	if err := json.NewDecoder(res.Body).Decode(out); err != nil {
		t.Fatal(err)
	}
}

// sendWelcome stands in for the application under test.
func sendWelcome(to string) error {
	msg := fmt.Sprintf("From: Acme <noreply@acme.test>\r\nTo: %s\r\nSubject: Welcome, your code is 731905\r\n\r\nWelcome aboard!\r\n", to)
	return smtp.SendMail(smtpAddr, nil, "noreply@acme.test", []string{to}, []byte(msg))
}

func TestWelcomeEmail(t *testing.T) {
	var cursor struct{ Seq int64 `json:"seq"` }
	api(t, http.MethodGet, "api/v2/events/cursor", nil, &cursor)

	if err := sendWelcome("carol+go@example.com"); err != nil {
		t.Fatal(err)
	}

	var msg struct {
		ID      string `json:"id"`
		Subject string `json:"subject"`
	}
	api(t, http.MethodPost, "api/v2/wait", map[string]any{
		"query": "to:carol+go@example.com", "since_seq": cursor.Seq, "timeout_ms": 10000,
	}, &msg)

	var codes []struct{ Value string `json:"value"` }
	api(t, http.MethodGet, "api/v2/messages/"+msg.ID+"/codes", nil, &codes)
	if len(codes) == 0 || codes[0].Value != "731905" {
		t.Fatalf("codes = %+v", codes)
	}
}
