package service

import (
	"time"

	pb "perfice.adoe.dev/proto"
)

// UnknownWebhookTokenError means no user integration owns the supplied token.
type UnknownWebhookTokenError struct{}

func (e UnknownWebhookTokenError) Error() string {
	return "unknown webhook token"
}

// MalformedPayloadError means the provider sent something that is not JSON.
type MalformedPayloadError struct{}

func (e MalformedPayloadError) Error() string {
	return "malformed webhook payload"
}

type IntegrationWebhookService struct {
	userIntegrationService *UserIntegrationService
	userService            pb.UserServiceClient
	processService         *IntegrationProcessService
	typeService            *IntegrationTypeService
}

func NewIntegrationWebhookService(userIntegrationService *UserIntegrationService, userService pb.UserServiceClient, processService *IntegrationProcessService,
	typeService *IntegrationTypeService) *IntegrationWebhookService {
	return &IntegrationWebhookService{userIntegrationService: userIntegrationService, userService: userService,
		processService: processService, typeService: typeService}
}

func (s *IntegrationWebhookService) HandleWebhook(token string, body []byte) error {
	integration, err := s.userIntegrationService.GetIntegrationByWebhookToken(token)
	if err != nil {
		return err
	}

	if integration == nil {
		return UnknownWebhookTokenError{}
	}

	timeZone, err := loadUserTimeZone(s.userService, integration.UserId)
	if err != nil {
		return err
	}

	definition := s.typeService.GetIntegrationEntityByIntegrationTypeAndEntityType(integration.IntegrationType, integration.EntityType)
	if definition == nil {
		return nil
	}

	now := time.Now().In(timeZone)

	return s.processService.handleIntegrationResponse(*definition, *integration, body, now)
}
