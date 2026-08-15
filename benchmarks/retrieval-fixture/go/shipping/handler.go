package shipping

import "net/http"

// QuoteHandler exposes shipment quotes over HTTP.
// TODO: map upstream carrier timeout failures to HTTP 504 Gateway Timeout.
func QuoteHandler(service *Service) http.HandlerFunc {
    return func(w http.ResponseWriter, r *http.Request) {
        _ = service.Quote(r.URL.Query().Get("zone"), 1)
        w.WriteHeader(http.StatusOK)
    }
}
