package httpx

import (
	"bytes"
	"errors"
	"io"
	"net/http"
	"regexp"
	"strconv"
	"strings"
	"unicode/utf8"

	"github.com/mihajlopopara/reqharbor/server/internal/jsonx"
	"github.com/mihajlopopara/reqharbor/server/internal/jsx"
)

// MaxBodyBytes is the 5 MiB request-body limit (PRODUCT §8; the client hard-codes it).
const MaxBodyBytes = 5 * 1024 * 1024

var jsonContentType = regexp.MustCompile(`(?i)^application/json\s*(;|$)`)

var utf8BOM = []byte{0xEF, 0xBB, 0xBF}

// ReadJSON reads and parses a JSON request body, checking in the Worker's order (§1.2):
// 415 Content-Type, 413 declared length, 400 missing body, 413 while streaming, 400 invalid UTF-8
// (a leading BOM is stripped), 400 blank body, 400 syntax.
func ReadJSON(w http.ResponseWriter, r *http.Request, maxBytes int64) (*jsonx.Value, error) {
	if !jsonContentType.MatchString(r.Header.Get("Content-Type")) {
		return nil, NewError(415, "unsupported_media_type", "Content-Type must be application/json")
	}
	tooLarge := TooLarge("payload_too_large", "Request body exceeds "+strconv.FormatInt(maxBytes, 10)+" bytes")
	if r.ContentLength > maxBytes {
		return nil, tooLarge
	}
	if r.Body == nil || r.Body == http.NoBody {
		return nil, BadRequestCode("invalid_json", "Request body is required")
	}
	body, err := io.ReadAll(http.MaxBytesReader(w, r.Body, maxBytes))
	if err != nil {
		var mbe *http.MaxBytesError
		if errors.As(err, &mbe) {
			return nil, tooLarge
		}
		return nil, BadRequestCode("invalid_json", "Request body could not be read")
	}
	body = bytes.TrimPrefix(body, utf8BOM)
	if !utf8.Valid(body) {
		return nil, BadRequestCode("invalid_json", "Request body is not valid UTF-8")
	}
	if strings.TrimFunc(string(body), jsx.IsJSSpace) == "" {
		return nil, BadRequestCode("invalid_json", "Request body is required")
	}
	v, err := jsonx.Parse(body)
	if err != nil {
		if errors.Is(err, jsonx.ErrTooDeep) {
			return nil, BadRequestCode("invalid_json", "Request body nests too deeply")
		}
		return nil, BadRequestCode("invalid_json", "Request body is not valid JSON")
	}
	return v, nil
}

// ReadJSONObject is ReadJSON followed by the Worker's requireObject: a value that is not an
// object is 400 validation_failed.
func ReadJSONObject(w http.ResponseWriter, r *http.Request, maxBytes int64) (*jsonx.Value, error) {
	v, err := ReadJSON(w, r, maxBytes)
	if err != nil {
		return nil, err
	}
	if !v.IsObject() {
		return nil, BadRequest("Request body must be a JSON object")
	}
	return v, nil
}
