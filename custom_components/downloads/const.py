"""Constants for the native Downloads integration."""

DOMAIN = "downloads"
CONF_BACKEND_URL = "backend_url"
CONF_TOKEN = "integration_token"
CONF_ADDON_SLUG = "addon_slug"
CONF_ALLOW_REQUESTS = "allow_requests"
MAX_REQUEST_BYTES = 64 * 1024
MAX_RESPONSE_BYTES = 2 * 1024 * 1024
REQUEST_TIMEOUT_SECONDS = 15
API_PREFIX = "/api/v1"

APP_URL = "https://github.com/jessfraz/ha-downloads"
HEALTH_PORT = 8080
