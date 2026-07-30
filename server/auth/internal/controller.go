package internal

import (
	"errors"
	"fmt"
	"net/mail"
	"strings"
	"time"

	"github.com/getsentry/sentry-go"
	"github.com/gofiber/fiber/v2"
	"go.mongodb.org/mongo-driver/bson/primitive"
)

type AuthController struct {
	authService    *AuthService
	sessionService *SessionService
}

type LoginRequest struct {
	Email    string `json:"email"`
	Password string `json:"password"`
}

type RegisterRequest struct {
	Email    string `json:"email"`
	Password string `json:"password"`
}

type SetTimezoneRequest struct {
	Timezone string `json:"timezone"`
}

type RefreshTokenRequest struct {
	AccessToken  string `json:"accessToken"`
	RefreshToken string `json:"refreshToken"`
}

type ResendConfirmationEmailRequest struct {
	Email string `json:"email"`
}

func sessionResponse(ctx *fiber.Ctx, session Session) error {
	return ctx.JSON(fiber.Map{
		"accessToken":  session.AccessToken,
		"refreshToken": session.RefreshToken,
	})
}

var userIdLocal string = "userId"
var sessionIdLocal string = "sessionId"

func getUserId(ctx *fiber.Ctx) string {
	return ctx.Locals(userIdLocal).(string)
}

func getSessionId(ctx *fiber.Ctx) string {
	return ctx.Locals(sessionIdLocal).(string)
}

func NewAuthController(authService *AuthService, sessionService *SessionService) *AuthController {
	return &AuthController{authService, sessionService}
}

func (c *AuthController) Register(ctx *fiber.Ctx) error {
	var request RegisterRequest
	if err := ctx.BodyParser(&request); err != nil {
		return ctx.SendStatus(fiber.StatusBadRequest)
	}

	email := c.sanitizeEmail(request.Email)
	if err := validateCredentials(email, request.Password); err != nil {
		return ctx.Status(fiber.StatusBadRequest).SendString(err.Error())
	}

	if err := c.authService.Register(email, request.Password); err != nil {
		if errors.Is(err, UserAlreadyExistsError{}) {
			return ctx.Status(fiber.StatusBadRequest).SendString("User already exists")
		} else {
			return err
		}
	}

	return ctx.SendStatus(fiber.StatusOK)
}

// sanitizeEmail produces the canonical stored form of an address.
//
// Only ASCII A-Z is folded. The previous implementation used strings.ToLower,
// a per-rune Unicode mapping, which is not a round trip: U+FB00 ("ff")
// uppercases to the two-character "FF", which lowercases to "ff". An address
// containing such a character, registered in uppercase, could never be logged
// into again -- and re-registering it silently created a second account.
//
// ASCII-only folding is round-trip safe by construction, matches what mail
// systems do in practice, and leaves every existing all-ASCII account
// unchanged.
func (c *AuthController) sanitizeEmail(email string) string {
	trimmed := strings.TrimSpace(email)

	var b strings.Builder
	b.Grow(len(trimmed))
	for i := 0; i < len(trimmed); i++ {
		ch := trimmed[i]
		if ch >= 'A' && ch <= 'Z' {
			ch += 'a' - 'A'
		}
		b.WriteByte(ch)
	}
	return b.String()
}

// isCanonicalTimezone screens out names that time.LoadLocation happens to
// resolve but that are not canonical IANA identifiers.
//
// Two cases matter. An empty string resolves to UTC, so a blank field used to
// be accepted and then stored verbatim, leaving the user on a timezone of ""
// that nothing can resolve later. And redundant separators such as
// "Europe//Amsterdam" resolve too, because LoadLocation ends up opening a
// filesystem path the OS normalises -- the stored value then differs from
// every other client's spelling of the same zone, and other tz libraries
// (notably Rust's chrono-tz) reject it outright.
func isCanonicalTimezone(timezone string) bool {
	if strings.TrimSpace(timezone) == "" {
		return false
	}

	// Rejects leading, trailing and doubled separators in one pass.
	for _, segment := range strings.Split(timezone, "/") {
		if segment == "" {
			return false
		}
	}

	return true
}

// MinPasswordLength is enforced at registration and password reset. Existing
// shorter passwords keep working; only new ones are checked.
const MinPasswordLength = 8

func validateCredentials(email string, password string) error {
	if _, err := mail.ParseAddress(email); err != nil {
		return errors.New("invalid email address")
	}

	if len(password) < MinPasswordLength {
		return fmt.Errorf("password must be at least %d characters", MinPasswordLength)
	}

	return nil
}

func (c *AuthController) Login(ctx *fiber.Ctx) error {
	var request LoginRequest
	if err := ctx.BodyParser(&request); err != nil {
		return ctx.SendStatus(fiber.StatusBadRequest)
	}

	session, err := c.authService.Login(c.sanitizeEmail(request.Email), request.Password)
	if err != nil {
		if errors.Is(err, UserNotConfirmedError{}) {
			return ctx.Status(fiber.StatusForbidden).SendString("Email not confirmed")
		}
		if errors.Is(err, InvalidCredentialsError{}) {
			return ctx.Status(fiber.StatusUnauthorized).SendString("Invalid username or password")
		}

		return err
	}

	return sessionResponse(ctx, session)
}

func (c *AuthController) Refresh(ctx *fiber.Ctx) error {
	var request RefreshTokenRequest
	if err := ctx.BodyParser(&request); err != nil {
		return ctx.SendStatus(fiber.StatusBadRequest)
	}

	session, err := c.sessionService.Refresh(request.AccessToken, request.RefreshToken)
	if err != nil {
		if errors.Is(err, InvalidSessionError{}) {
			return ctx.Status(fiber.StatusUnauthorized).SendString("Invalid session")
		}
		return err
	}

	return sessionResponse(ctx, session)
}

func (c *AuthController) Logout(ctx *fiber.Ctx) error {
	sessionId := getSessionId(ctx)
	if err := c.sessionService.Logout(sessionId); err != nil {
		return ctx.Status(fiber.StatusBadRequest).SendString("Invalid session")
	}

	return ctx.SendStatus(fiber.StatusOK)
}

func (c *AuthController) SetTimezone(ctx *fiber.Ctx) error {
	var request SetTimezoneRequest
	if err := ctx.BodyParser(&request); err != nil {
		return ctx.SendStatus(fiber.StatusBadRequest)
	}

	userId := getUserId(ctx)

	if !isCanonicalTimezone(request.Timezone) {
		return ctx.Status(fiber.StatusBadRequest).SendString("Invalid timezone")
	}

	_, err := time.LoadLocation(request.Timezone)
	if err != nil {
		return ctx.Status(fiber.StatusBadRequest).SendString("Invalid timezone")
	}

	err = c.authService.SetTimezone(userId, request.Timezone)
	if err != nil {
		return err
	}

	return ctx.SendStatus(fiber.StatusOK)
}

func (c *AuthController) Me(ctx *fiber.Ctx) error {
	userId := getUserId(ctx)
	timezone, err := c.authService.GetUserTimeZone(userId)
	if err != nil {
		return err
	}

	return ctx.JSON(fiber.Map{
		"id":       userId,
		"timezone": timezone,
	})
}

func (c *AuthController) DeleteAccount(ctx *fiber.Ctx) error {
	userId := getUserId(ctx)
	if err := c.authService.DeleteUser(userId); err != nil {
		return err
	}

	return ctx.SendStatus(fiber.StatusOK)
}

func (c *AuthController) ConfirmEmail(ctx *fiber.Ctx) error {
	token, err := primitive.ObjectIDFromHex(ctx.Params("token"))
	if err != nil {
		return ctx.Status(fiber.StatusBadRequest).SendString("Invalid token")
	}

	err = c.authService.ConfirmEmail(token)
	if err != nil {
		sentry.CaptureException(err)
		return ctx.Status(fiber.StatusBadRequest).SendString("Invalid token")
	}

	return ctx.Type("html").SendString(fmt.Sprintf(confirmEmailHtml, appBaseUrl))
}

func (c *AuthController) InitResetPassword(ctx *fiber.Ctx) error {
	email := ctx.Query("email")
	err := c.authService.InitResetPassword(c.sanitizeEmail(email))
	if err != nil {
		sentry.CaptureException(err)
		return ctx.Status(fiber.StatusBadRequest).SendString("Invalid token")
	}

	return ctx.SendStatus(fiber.StatusOK)
}
func (c *AuthController) FillResetPassword(ctx *fiber.Ctx) error {
	token, err := primitive.ObjectIDFromHex(ctx.Params("token"))
	if err != nil {
		return ctx.Status(fiber.StatusBadRequest).SendString("Invalid token")
	}

	if !c.authService.ValidateResetPassword(token) {
		sentry.CaptureException(err)
		return ctx.Status(fiber.StatusBadRequest).SendString("Invalid token")
	}

	return ctx.Type("html").SendString(fmt.Sprintf(resetPasswordInitHtml, token.Hex()))
}

func (c *AuthController) ResetPassword(ctx *fiber.Ctx) error {
	token, err := primitive.ObjectIDFromHex(ctx.FormValue("token"))
	if err != nil {
		return ctx.Status(fiber.StatusBadRequest).SendString("Invalid token")
	}

	password := ctx.FormValue("password")
	err = c.authService.ResetPassword(token, password)
	if err != nil {
		sentry.CaptureException(err)
		return ctx.Status(fiber.StatusBadRequest).SendString("Invalid token")
	}

	return ctx.Type("html").SendString(fmt.Sprintf(resetPasswordHtml, appBaseUrl))
}

func (c *AuthController) ResendConfirmationEmail(ctx *fiber.Ctx) error {
	var request ResendConfirmationEmailRequest
	if err := ctx.BodyParser(&request); err != nil {
		return ctx.SendStatus(fiber.StatusBadRequest)
	}

	err := c.authService.ResendConfirmationEmail(c.sanitizeEmail(request.Email))
	if err != nil {
		sentry.CaptureException(err)
		return ctx.SendStatus(fiber.StatusBadRequest)
	}

	return ctx.SendStatus(fiber.StatusOK)
}

type FeedbackController struct {
	feedbackService *FeedbackService
}

func NewFeedbackController(feedbackService *FeedbackService) *FeedbackController {
	return &FeedbackController{feedbackService}
}

// MaxFeedbackLength bounds a deliberately unauthenticated endpoint. Feedback
// has to stay anonymous -- users report problems they cannot log in to
// describe -- so the protection is a size cap rather than a credential.
const MaxFeedbackLength = 4096

func (c *FeedbackController) Feedback(ctx *fiber.Ctx) error {
	feedback := ctx.Body()
	if len(feedback) == 0 {
		return ctx.Status(fiber.StatusBadRequest).SendString("Feedback is empty")
	}

	if len(feedback) > MaxFeedbackLength {
		return ctx.Status(fiber.StatusBadRequest).SendString("Feedback is too long")
	}

	if err := c.feedbackService.Insert(string(feedback)); err != nil {
		return err
	}

	return ctx.SendStatus(fiber.StatusOK)
}
