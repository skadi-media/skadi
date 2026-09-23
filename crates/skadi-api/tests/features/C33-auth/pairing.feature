Feature: C33 LAN trust model, pairing and app distribution
  Deliberate constraint (deploy/README "Web UI"): the daemon injects the API
  token into the served index.html, so any browser that can load `/` gets the
  token. The stack is bound to loopback / home LAN and never port-forwarded.
  The Android app pairs by scanning a `skadi://pair` QR that carries host+token.

  @C33 @passing
  Scenario: the UI shell is served to an unauthenticated visitor
    Given a daemon protected by API token "s3cret"
    And the client presents no token
    When the client requests GET "/"
    Then the response status is 200
    And the response body is an HTML document

  @C33 @passing
  Scenario: an unknown client-side route still serves the UI shell, but API paths never do
    Given a daemon protected by API token "s3cret"
    And the client presents no token
    When the client requests GET "/movies/some-id"
    Then the response status is 200
    And the response body is an HTML document
    When the client requests GET "/api/v1/no-such-route"
    Then the response status is 404
    And the response body is not an HTML document

  @C33 @passing
  Scenario: the web-player pairing QR encodes the address the browser reached us at
    Given a daemon protected by API token "s3cret"
    And the client presents token "s3cret"
    And the client presents the header "host" with value "203.0.113.50:8090"
    When the client requests GET "/api/v1/pair/qr"
    Then the response status is 200
    And the response field "url" is "http://203.0.113.50:8090/listen"
    And the response field "qr_svg" contains "<svg"

  @C33 @passing
  Scenario: the pairing QR honours an explicit url override
    Given a daemon protected by API token "s3cret"
    And the client presents token "s3cret"
    When the client requests GET "/api/v1/pair/qr?url=https://skadi.example/listen"
    Then the response field "url" is "https://skadi.example/listen"

  @C33 @passing
  Scenario: the native-app pairing QR hands the bearer token to whoever can read it
    Given a daemon protected by API token "s3cret"
    And the client presents token "s3cret"
    And the client presents the header "host" with value "203.0.113.27:8090"
    When the client requests GET "/api/v1/pair/app"
    Then the response status is 200
    And the response field "url" is "skadi://pair?host=203.0.113.27%3A8090&token=s3cret"

  @C33 @passing
  Scenario: pairing endpoints themselves require the token
    Given a daemon protected by API token "s3cret"
    And the client presents no token
    When the client requests GET "/api/v1/pair/app"
    Then the response status is 401

  @C33 @passing @serial
  Scenario: SKADI_ADVERTISE_HOST wins over the Host header for containerised daemons
    Given a daemon protected by API token "s3cret"
    And the environment variable "SKADI_ADVERTISE_HOST" is "skadi.lan:8090"
    And the client presents token "s3cret"
    And the client presents the header "host" with value "172.18.0.5:8080"
    When the client requests GET "/api/v1/pair/qr"
    Then the response field "url" is "http://skadi.lan:8090/listen"
    Given the environment variable "SKADI_ADVERTISE_HOST" is unset

  @C33 @passing @serial
  Scenario: APK distribution is unauthenticated and reports nothing when no build is published
    Given a daemon protected by API token "s3cret"
    And the environment variable "SKADI_APK_DIR" is unset
    And the client presents no token
    When the client requests GET "/app/skadi.apk"
    Then the response status is 404
    Given the client presents token "s3cret"
    When the client requests GET "/api/v1/pair/apk"
    Then the response status is 200
    And the response field "available" is "false"

  @C33 @passing @serial
  Scenario: a published APK is advertised with an install URL and streamed without a token
    Given a daemon protected by API token "s3cret"
    And a published APK "skadi-1.2.3.apk" with manifest version "1.2.3" in the scratch apk folder
    And the environment variable "SKADI_APK_DIR" is "<tmp>/apk"
    And the client presents token "s3cret"
    And the client presents the header "host" with value "203.0.113.27:8090"
    When the client requests GET "/api/v1/pair/apk"
    Then the response field "available" is "true"
    And the response field "version_name" is "1.2.3"
    And the response field "url" is "http://203.0.113.27:8090/app/skadi-1.2.3.apk"
    Given the client presents no token
    When the client requests GET "/app/skadi-1.2.3.apk"
    Then the response status is 200
    And the response header "content-type" is "application/vnd.android.package-archive"
    When the client requests GET "/app/..%2Fmanifest.json"
    Then the response status is 404
    Given the environment variable "SKADI_APK_DIR" is unset
