// Package httpx holds the HTTP plumbing shared by every route: the JSON error envelope, JSON
// bodies, the method dispatcher and the middleware (06-go-server.md §1.2).
package httpx

import (
	"bytes"
	"encoding/json"
	"errors"
	"net/http"
	"strconv"
)

// APIError is an error with a status, a stable code and a message, written as
// {"error":{"code","message"}}.
type APIError struct {
	Status  int
	Code    string
	Message string
	Headers map[string]string
}

func (e *APIError) Error() string { return e.Code + ": " + e.Message }

// WithHeader returns e with an extra response header.
func (e *APIError) WithHeader(k, v string) *APIError {
	h := make(map[string]string, len(e.Headers)+1)
	for a, b := range e.Headers {
		h[a] = b
	}
	h[k] = v
	c := *e
	c.Headers = h
	return &c
}

// NewError returns an APIError.
func NewError(status int, code, message string) *APIError {
	return &APIError{Status: status, Code: code, Message: message}
}

// BadRequest is 400 validation_failed.
func BadRequest(message string) *APIError { return NewError(400, "validation_failed", message) }

// BadRequestCode is 400 with a specific code.
func BadRequestCode(code, message string) *APIError { return NewError(400, code, message) }

// NotFound is 404 not_found.
func NotFound(message string) *APIError { return NewError(404, "not_found", message) }

// Gone is 410 gone with the Worker's share message.
func Gone() *APIError { return NewError(410, "gone", "This share link has been revoked") }

// TooLarge is 413 with the given code.
func TooLarge(code, message string) *APIError { return NewError(413, code, message) }

// RouteNotFound is the catch-all 404.
func RouteNotFound(r *http.Request) *APIError {
	return NotFound("No route for " + r.Method + " " + r.URL.EscapedPath())
}

// Internal is the generic 500; details go to the log only.
func Internal() *APIError { return NewError(500, "internal_error", "Internal server error") }

// RateLimited is 429 rate_limited with Retry-After in whole seconds (at least 1).
func RateLimited(message string, retryAfterSeconds int64) *APIError {
	if retryAfterSeconds < 1 {
		retryAfterSeconds = 1
	}
	return NewError(429, "rate_limited", message).WithHeader("Retry-After", strconv.FormatInt(retryAfterSeconds, 10))
}

// ContentTypeJSON is the Content-Type of every JSON response.
const ContentTypeJSON = "application/json; charset=utf-8"

// Marshal encodes v like JSON.stringify: no HTML escaping and no trailing newline.
func Marshal(v any) ([]byte, error) {
	var b bytes.Buffer
	enc := json.NewEncoder(&b)
	enc.SetEscapeHTML(false)
	if err := enc.Encode(v); err != nil {
		return nil, err
	}
	out := b.Bytes()
	return out[:len(out)-1], nil
}

// WriteJSON writes v with the given status and extra headers.
func WriteJSON(w http.ResponseWriter, status int, v any, headers map[string]string) error {
	body, err := Marshal(v)
	if err != nil {
		return err
	}
	h := w.Header()
	h.Set("Content-Type", ContentTypeJSON)
	for k, val := range headers {
		h.Set(k, val)
	}
	h.Set("Content-Length", strconv.Itoa(len(body)))
	w.WriteHeader(status)
	_, _ = w.Write(body)
	return nil
}

type errorBody struct {
	Error struct {
		Code    string `json:"code"`
		Message string `json:"message"`
	} `json:"error"`
}

// WriteError writes e as the JSON error envelope.
func WriteError(w http.ResponseWriter, e *APIError) {
	var b errorBody
	b.Error.Code = e.Code
	b.Error.Message = e.Message
	_ = WriteJSON(w, e.Status, b, e.Headers)
}

// AsAPIError returns err as an *APIError, if it is one.
func AsAPIError(err error) (*APIError, bool) {
	var e *APIError
	if errors.As(err, &e) {
		return e, true
	}
	return nil, false
}
