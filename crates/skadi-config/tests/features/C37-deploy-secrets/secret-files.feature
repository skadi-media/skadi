Feature: secrets from files (SKADI_X_FILE)
  A secret passed as a plain environment variable shows in `docker inspect`.
  Every SKADI_ variable may instead name a file with SKADI_X_FILE, the Docker
  secrets convention. The plain variable keeps working unchanged.

  @C37 @passing @serial
  Scenario: the boot seeder reads a Tier-1 secret from its file
    Given the environment variable "SKADI_API_TOKEN" is unset
    And the secret file "skadi_api_token" holds the line "tok-from-file"
    And the environment variable "SKADI_API_TOKEN_FILE" names the secret file "skadi_api_token"
    When the boot seeder collects the environment
    Then the seeder collected "api_token" as "tok-from-file"

  @C37 @passing @serial
  Scenario: a plain variable still works when no file variable is set
    Given the environment variable "SKADI_API_TOKEN_FILE" is unset
    And the environment variable "SKADI_API_TOKEN" is "tok-plain"
    When the boot seeder collects the environment
    Then the seeder collected "api_token" as "tok-plain"

  @C37 @passing @serial
  Scenario: setting a secret both ways is refused without printing it
    Given the environment variable "SKADI_API_TOKEN" is "plain-value-123"
    And the secret file "skadi_api_token" holds the line "file-value-456"
    And the environment variable "SKADI_API_TOKEN_FILE" names the secret file "skadi_api_token"
    When the boot seeder collects the environment
    Then the boot seeder fails, naming "SKADI_API_TOKEN_FILE"
    And the error does not contain "plain-value-123"
    And the error does not contain "file-value-456"

  @C37 @passing @serial
  Scenario: a file variable that names a missing file is refused
    Given the environment variable "SKADI_API_TOKEN" is unset
    And the environment variable "SKADI_API_TOKEN_FILE" names the secret file "absent"
    When the boot seeder collects the environment
    Then the boot seeder fails, naming "SKADI_API_TOKEN_FILE"
