package httpx

import (
	"errors"
	"net/http"
	"strings"
)

// Func is a handler that may fail. An *APIError is written as JSON; any other error becomes a
// logged 500 internal_error without details.
type Func func(w http.ResponseWriter, r *http.Request) error

// ServeHTTP implements http.Handler.
func (f Func) ServeHTTP(w http.ResponseWriter, r *http.Request) {
	if err := f(w, r); err != nil {
		HandleError(w, r, err)
	}
}

// HandleError writes err: an *APIError as is, anything else as a logged 500. When the handler
// already started the response, nothing more can be sent: the error is only logged. A failure
// caused by the client going away (its context is canceled) is logged at debug level, since it is
// not a server fault and the response goes nowhere.
func HandleError(w http.ResponseWriter, r *http.Request, err error) {
	log := LoggerFrom(r.Context())
	if started(w) {
		log.Error("error after the response started", "err", err.Error())
		return
	}
	if e, ok := AsAPIError(err); ok {
		WriteError(w, e)
		return
	}
	if r.Context().Err() != nil && errors.Is(err, r.Context().Err()) {
		log.Debug("request canceled by the client", "err", err.Error())
		WriteError(w, Internal())
		return
	}
	log.Error("unhandled error", "err", err.Error())
	WriteError(w, Internal())
}

// started reports whether w (the security-header wrapper, possibly wrapped again) has already
// written its header.
func started(w http.ResponseWriter) bool {
	for {
		switch x := w.(type) {
		case *headerWriter:
			return x.wrote
		case interface{ Unwrap() http.ResponseWriter }:
			w = x.Unwrap()
		default:
			return false
		}
	}
}

// Method is one entry of a Methods dispatch table.
type Method struct {
	Name    string
	Handler http.Handler
}

// Methods dispatches on the request method. A method that is not listed gets 405
// method_not_allowed with `Allow` listing the methods in registration order; HEAD is never added
// implicitly (unlike ServeMux method patterns).
type Methods []Method

// ServeHTTP implements http.Handler.
func (m Methods) ServeHTTP(w http.ResponseWriter, r *http.Request) {
	for _, e := range m {
		if e.Name == r.Method {
			e.Handler.ServeHTTP(w, r)
			return
		}
	}
	names := make([]string, len(m))
	for i, e := range m {
		names[i] = e.Name
	}
	allow := strings.Join(names, ", ")
	WriteError(w, NewError(405, "method_not_allowed", "Method "+r.Method+" not allowed; use "+allow).WithHeader("Allow", allow))
}

// NotFoundHandler is the catch-all route.
var NotFoundHandler = Func(func(w http.ResponseWriter, r *http.Request) error { return RouteNotFound(r) })
