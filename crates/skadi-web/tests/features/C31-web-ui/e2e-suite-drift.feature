Feature: C31 existing browser E2E suites stay green
  `web-e2e/tests/steer.spec.ts` and `audiobooks.spec.ts` are the only browser
  E2E. Both seed the harness with the single library root, which SKADI-T-0302
  made the config key `library.root` — not the `root_folders` settings kind it
  replaced. Seeding the removed kind 404s, and the `expect(...ok())` on it fails
  before any UI is exercised, which is how both suites stayed red from T-0302
  until SKADI-T-0461.

  @C31 @passing @SKADI-T-0461
  Scenario: the steer-it E2E spec seeds a root folder the API accepts
    Given the harness daemon with the movies domain enabled
    When the steer-it spec seeds its root folder
    Then the seeding request is accepted

  @C31 @passing @SKADI-T-0461
  Scenario: seeding uses the config key, not the removed settings kind
    Given the harness daemon with the movies domain enabled
    When the steer-it spec seeds its root folder
    Then it sets the config key "library.root"
    And it does not call the removed "root_folders" settings kind
