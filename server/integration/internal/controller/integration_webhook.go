package controller

import (
	"errors"

	"github.com/gofiber/fiber/v2"
	"perfice.adoe.dev/integration/internal/service"
)

type IntegrationWebhookController struct {
	service *service.IntegrationWebhookService
}

func NewIntegrationWebhookController(service *service.IntegrationWebhookService) *IntegrationWebhookController {
	return &IntegrationWebhookController{service}
}

func (c *IntegrationWebhookController) HandleWebhook(ctx *fiber.Ctx) error {
	token := ctx.Params("token")
	body := ctx.Body()

	if err := (*c.service).HandleWebhook(token, body); err != nil {
		// Both of these are the caller's problem, and providers retry on 5xx --
		// answering 500 to a permanently bad request means retrying forever.
		if errors.Is(err, service.UnknownWebhookTokenError{}) {
			return ctx.SendStatus(fiber.StatusNotFound)
		}

		if errors.Is(err, service.MalformedPayloadError{}) {
			return ctx.Status(fiber.StatusBadRequest).SendString("Malformed payload")
		}

		return err
	}
	return ctx.SendStatus(fiber.StatusOK)
}
