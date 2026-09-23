Feature: C31 app shell, navigation and token injection
  The daemon serves the SPA at `/` and injects SKADI_API_TOKEN into
  `<meta name="skadi-api-token">`; the UI reads it and sends it as the bearer on
  every /api/v1 call. LAN trust model: any visitor who can load `/` gets the
  token (deliberate; the stack is never port-forwarded).

  @C31 @passing
  Scenario: the shell loads and the sidebar lists the enabled domains only
    Given the harness daemon with the movies domain enabled
    When the operator opens "/"
    Then the sidebar shows a link "Overview"
    And the sidebar shows a link "Movies"
    And the sidebar does not show a link "Television"

  @C31 @passing
  Scenario: every UI API call carries the injected bearer token
    Given the harness daemon with the movies domain enabled
    When the operator opens "/movies"
    Then every request the page made to "/api/v1/" carried an Authorization header

  @C31 @passing
  Scenario: an unknown client-side route renders the shell, not a daemon 404
    Given the harness daemon with the movies domain enabled
    When the operator opens "/no/such/page"
    Then the sidebar shows a link "Overview"

  @C31 @passing @SKADI-T-0474
  Scenario: an expired or rotated token is surfaced as a re-authenticate prompt
    Given the harness daemon with the movies domain enabled
    And the daemon's api_token is rotated after the page loaded
    When the operator opens "/movies"
    Then the page shows a banner containing "sign in"
