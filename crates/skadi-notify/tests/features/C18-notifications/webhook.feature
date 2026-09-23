Feature: Webhook notifier — delivery, signing, channel filtering and the test button
  crates/skadi-notify/src/webhook.rs is the only notifier kind. Sonarr/Radarr
  equivalent: Connect → Webhook (plus Discord/Telegram/Pushover/… and a per-
  connection Test button that sends a real test event). The receiver is an
  in-process mock.

  @C18 @passing
  Scenario: a grabbed event is posted as a JSON envelope with the event tag, the payload and a timestamp
    Given a webhook notifier subscribed to "grabbed,imported"
    And the webhook endpoint answers HTTP 200
    When a "grabbed" event for "Heat" (1995) at "Bluray-1080p" is dispatched
    Then the event is delivered
    And the endpoint received 1 POST
    And the posted JSON has "event" = "grabbed"
    And the posted JSON has "payload.title" = "Heat"
    And the posted JSON has "payload.year" = "1995"
    And the posted JSON has "payload.quality" = "Bluray-1080p"
    And the posted JSON has no "payload.message"
    And the posted JSON carries an RFC3339 timestamp

  @C18 @passing
  Scenario: with a shared secret the body is signed with HMAC-SHA256 in x-skadi-signature
    Given a webhook notifier subscribed to "imported" signed with "topsecret"
    And the webhook endpoint answers HTTP 200
    When a "imported" event for "The Matrix" (1999) at "Bluray-1080p" is dispatched
    Then the event is delivered
    And the POST is JSON and carries a valid HMAC-SHA256 signature of its body

  @C18 @passing
  Scenario: without a secret the request is unsigned
    Given a webhook notifier subscribed to "imported"
    And the webhook endpoint answers HTTP 204
    When a "imported" event for "The Matrix" is dispatched
    Then the event is delivered
    And the POST carries no signature header

  @C18 @passing
  Scenario: events outside the subscribed channels are filtered out before any request
    Given a webhook notifier subscribed to "imported"
    And the webhook endpoint answers HTTP 200
    When a "grabbed" event for "Heat" is dispatched
    Then the event is filtered out before any request is made
    And the notifier wants "imported" events
    And the notifier does not want "health" events

  @C18 @passing
  Scenario: a non-2xx answer from the receiver is a delivery error
    Given a webhook notifier subscribed to "failed"
    And the webhook endpoint answers HTTP 500
    When a "failed" event for "Heat" is dispatched
    Then the delivery fails with an error containing "HTTP 500"

  @C18 @passing
  Scenario: every event kind serialises with its snake_case tag and omits empty fields
    Then a "grabbed" event serialises with tag "grabbed" and no null fields
    And a "imported" event serialises with tag "imported" and no null fields
    And a "upgraded" event serialises with tag "upgraded" and no null fields
    And a "failed" event serialises with tag "failed" and no null fields
    And a "health" event serialises with tag "health" and no null fields

  @C18 @passing @SKADI-T-0501
  Scenario: the test button sends a real test event to the receiver and fails when the receiver is down
    Given a webhook notifier subscribed to "imported"
    And the webhook endpoint answers HTTP 200
    When the notifier's test button is pressed
    Then the test passes
    And the endpoint received 1 POST
    And the posted JSON has "event" = "test"

  @C18 @passing @SKADI-T-0504
  Scenario: a transient receiver failure is retried (Sonarr retries webhooks with backoff)
    Given a webhook notifier subscribed to "imported"
    And the webhook endpoint answers HTTP 503 once and then HTTP 200
    When a "imported" event for "The Matrix" is dispatched
    Then the event is delivered
    And the endpoint received 2 POSTs
