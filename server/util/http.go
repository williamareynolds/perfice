package util

import (
	"errors"
	"os"

	"github.com/go-playground/validator/v10"
	"github.com/gofiber/fiber/v2"
)

// InternalSecretHeader carries the shared secret the gateway attaches to every
// request it proxies. The backends trust X-Userid / X-Sessionid without
// verification, so this header is what stops an accidentally exposed backend
// port from being an instant account-impersonation hole.
const InternalSecretHeader = "X-Internal-Secret"

// InternalSecretEnv is the environment variable holding that secret. It must be
// identical for the gateway and every backend.
const InternalSecretEnv = "INTERNAL_SECRET"

// RequireInternalSecret reads the shared secret and fails fast when it is
// missing.
//
// Failing at boot is deliberate: a service that silently started without the
// check would look healthy while accepting unauthenticated identity headers.
func RequireInternalSecret() string {
	secret := os.Getenv(InternalSecretEnv)
	if secret == "" {
		panic(InternalSecretEnv + " is not set; refusing to start (see server/README.md)")
	}
	return secret
}

// InternalSecretMiddleware rejects any request that did not come through the
// gateway.
func InternalSecretMiddleware(secret string) fiber.Handler {
	return func(c *fiber.Ctx) error {
		provided := GetFromMapOrNil(c.GetReqHeaders(), InternalSecretHeader)
		if provided == nil || len(*provided) != 1 || (*provided)[0] != secret {
			return c.SendStatus(fiber.StatusUnauthorized)
		}
		return c.Next()
	}
}

// NewErrorHandler builds the Fiber error handler used by every service.
//
// It replaces the previous handler, which discarded the error entirely and
// always sent 500. That collapsed three very different things into one status:
// a routing miss (404), a malformed request (400) and a genuine server fault.
// Clients could not tell them apart, and neither could logs.
//
// report receives only the errors that indicate a real fault, so client
// mistakes no longer generate error-tracker noise.
func NewErrorHandler(report func(error)) fiber.ErrorHandler {
	return func(ctx *fiber.Ctx, err error) error {
		if status, ok := clientErrorStatus(err); ok {
			return ctx.SendStatus(status)
		}

		// Anything unrecognised is ours, not the caller's.
		if report != nil {
			report(err)
		}
		return ctx.SendStatus(fiber.StatusInternalServerError)
	}
}

// clientErrorStatus maps an error to a 4xx status when it was caused by the
// caller, reporting false when the error is a server fault.
func clientErrorStatus(err error) (int, bool) {
	// Validation failures are always the caller's fault.
	var validationErrors validator.ValidationErrors
	if errors.As(err, &validationErrors) {
		return fiber.StatusBadRequest, true
	}

	var invalidValidation *validator.InvalidValidationError
	if errors.As(err, &invalidValidation) {
		// Misuse of the validator itself: that is a programming error.
		return 0, false
	}

	// Fiber raises routing misses ("Cannot GET /x") and body-parser failures as
	// *fiber.Error with a meaningful status already attached.
	var fiberErr *fiber.Error
	if errors.As(err, &fiberErr) {
		if fiberErr.Code >= 400 && fiberErr.Code < 500 {
			return fiberErr.Code, true
		}
		return 0, false
	}

	return 0, false
}
