Feature: Notifier settings rows and the notification event vocabulary
  crates/skadi-notify/src/config.rs (kind-tagged NotifierConfig) and lib.rs
  (NotificationEvent). Sonarr's triggers: On Grab, On Import, On Upgrade, On
  Rename, On Series/Movie Delete, On Episode/Movie File Delete, On Health Issue,
  On Health Restored, On Application Update, On Manual Interaction Required.

  @C18 @passing
  Scenario: a webhook row builds and round-trips
    Given the notifier settings row:
      """
      { "kind": "webhook", "name": "ping", "url": "https://example.test/hook", "channels": ["imported", "failed"] }
      """
    When the provider factory builds the notifier
    Then the notifier builds
    And the settings row round-trips
    And the notifier wants "failed" events
    And the notifier does not want "grabbed" events

  @C18 @passing
  Scenario: a webhook row needs an http(s) URL
    Given the notifier settings row:
      """
      { "kind": "webhook", "name": "ping", "url": "nope", "channels": ["imported"] }
      """
    When the provider factory builds the notifier
    Then the build is rejected with a validation error mentioning "url"

  @C18 @passing
  Scenario: a webhook row needs at least one channel
    Given the notifier settings row:
      """
      { "kind": "webhook", "name": "ping", "url": "https://example.test/hook", "channels": [] }
      """
    When the provider factory builds the notifier
    Then the build is rejected with a validation error mentioning "channels"

  @C18 @passing
  Scenario: the five existing event kinds deserialise from their tags
    Given the event JSON:
      """
      { "event": "upgraded", "title": "Heat", "year": 1995, "quality": "Bluray-1080p" }
      """
    Then it deserialises as a "upgraded" notification event
    Given the event JSON:
      """
      { "event": "health", "title": "indexer down", "message": "3 consecutive failures" }
      """
    Then it deserialises as a "health" notification event

  @C18 @passing @SKADI-T-0504
  Scenario: rename and file-delete events exist in the vocabulary (Sonarr's On Rename / On Delete triggers)
    Given the event JSON:
      """
      { "event": "renamed", "title": "Heat" }
      """
    Then it deserialises as a "renamed" notification event
    Given the event JSON:
      """
      { "event": "deleted", "title": "Heat" }
      """
    Then it deserialises as a "deleted" notification event

  @C18 @passing @SKADI-T-0504
  Scenario: a health-restored event exists so a receiver can clear an alert
    Given the event JSON:
      """
      { "event": "health_restored", "title": "indexer back" }
      """
    Then it deserialises as a "health_restored" notification event

  @C18 @passing @SKADI-T-0504
  Scenario: a Discord notifier kind can be configured (Sonarr ships Discord/Telegram/Pushover/Email/…)
    Given the notifier settings row:
      """
      { "kind": "discord", "name": "chat", "url": "https://discord.test/api/webhooks/1/x", "channels": ["imported"] }
      """
    When the provider factory builds the notifier
    Then the notifier builds

  @C18 @passing @SKADI-T-0538
  Scenario: a Telegram notifier keeps its bot token in the credential store, not the settings body
    Given the notifier settings row:
      """
      { "kind": "telegram", "name": "phone", "chat_id": "-100123", "channels": ["imported"] }
      """
    When the provider factory builds the notifier with secret "bot-token"
    Then the notifier builds

  @C18 @passing @SKADI-T-0538
  Scenario: a Telegram notifier without its bot token is refused rather than built half-configured
    Given the notifier settings row:
      """
      { "kind": "telegram", "name": "phone", "chat_id": "-100123", "channels": ["imported"] }
      """
    When the provider factory builds the notifier without a secret
    Then the notifier is rejected naming the field "secret"

  @C18 @passing @SKADI-T-0538
  Scenario: a Pushover notifier can be configured
    Given the notifier settings row:
      """
      { "kind": "pushover", "name": "phone", "user_key": "uQiRz", "channels": ["imported", "upgraded"] }
      """
    When the provider factory builds the notifier with secret "app-token"
    Then the notifier builds
