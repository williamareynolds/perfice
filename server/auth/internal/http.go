package internal

import (
	"fmt"
	"log"
	"os"

	"github.com/getsentry/sentry-go"
	jwtware "github.com/gofiber/contrib/jwt"
	"github.com/gofiber/fiber/v2"
	"github.com/gofiber/fiber/v2/middleware/cors"
	"github.com/gofiber/fiber/v2/middleware/recover"
	"github.com/golang-jwt/jwt/v5"
	"perfice.adoe.dev/util"
)

func (a *AuthApp) setupHttpServer(secret []byte, authService *AuthService, sessionService *SessionService, feedbackService *FeedbackService) {
	app := fiber.New(fiber.Config{
		ErrorHandler: util.NewErrorHandler(func(err error) {
			log.Println("Error occurred:", err)
			sentry.CaptureException(err)
		}),
	})

	app.Use(recover.New(
		recover.Config{
			EnableStackTrace: true,
		}))

	// Every request must have come through the gateway. Registered before any
	// route so it also covers the unauthenticated ones.
	app.Use(util.InternalSecretMiddleware(util.RequireInternalSecret()))

	app.Use(cors.New(cors.Config{
		AllowOrigins:     "https://localhost, http://localhost:8000, http://localhost:5173, https://perfice.adoe.dev",
		AllowHeaders:     "content-type, authorization",
		AllowCredentials: true,
	}))

	jwtMiddleware := jwtware.New(jwtware.Config{
		SigningKey: jwtware.SigningKey{Key: secret},
	})

	authController := NewAuthController(authService, sessionService)
	// jwtMiddleware only proves the signature is ours; sessionMiddleware proves
	// the session behind it has not been revoked.
	sessionMiddleware := newSessionMiddleware(sessionService)
	authenticated := []fiber.Handler{jwtMiddleware, authMiddleware, sessionMiddleware}

	app.Post("/register", authController.Register)
	app.Post("/login", authController.Login)
	app.Post("/refresh", authController.Refresh)
	app.Put("/timezone", append(authenticated, authController.SetTimezone)...)

	app.Get("/me", append(authenticated, authController.Me)...)
	app.Post("/delete", append(authenticated, authController.DeleteAccount)...)
	app.Post("/logout", append(authenticated, authController.Logout)...)
	app.Get("/confirm/:token", authController.ConfirmEmail)
	app.Post("/resetInit", authController.InitResetPassword)
	app.Post("/reset", authController.ResetPassword)
	app.Post("/resendConfirm", authController.ResendConfirmationEmail)
	app.Get("/reset/:token", authController.FillResetPassword)

	feedbackController := NewFeedbackController(feedbackService)
	app.Post("/feedback", feedbackController.Feedback)

	port := os.Getenv("HTTP_PORT")
	fmt.Println("Serving HTTP on port " + port)
	err := app.Listen(":" + port)
	if err != nil {
		panic(err)
	}
}

func authMiddleware(c *fiber.Ctx) error {
	user := c.Locals("user")
	if user == nil {
		return c.SendStatus(fiber.StatusUnauthorized)
	}

	token := user.(*jwt.Token)
	claims := token.Claims.(jwt.MapClaims)

	userId := util.GetFromMapOrNil(claims, "sub")
	sessionId := util.GetFromMapOrNil(claims, "session")

	if userId == nil || sessionId == nil {
		return c.SendStatus(fiber.StatusUnauthorized)
	}

	c.Locals(userIdLocal, *userId)
	c.Locals(sessionIdLocal, *sessionId)
	return c.Next()
}

// newSessionMiddleware rejects tokens whose session has been logged out or
// deleted. Must run after authMiddleware, which populates the locals.
func newSessionMiddleware(sessionService *SessionService) fiber.Handler {
	return func(c *fiber.Ctx) error {
		userId, ok := c.Locals(userIdLocal).(string)
		if !ok {
			return c.SendStatus(fiber.StatusUnauthorized)
		}

		sessionId, ok := c.Locals(sessionIdLocal).(string)
		if !ok {
			return c.SendStatus(fiber.StatusUnauthorized)
		}

		if err := sessionService.RequireLiveSession(userId, sessionId); err != nil {
			return c.SendStatus(fiber.StatusUnauthorized)
		}

		return c.Next()
	}
}
