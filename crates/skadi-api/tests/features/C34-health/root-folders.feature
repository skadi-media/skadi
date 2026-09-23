Feature: C34 library root diagnostics
  Skadi owns a single `library.root`; `/root-folders` reports its health and
  free space, `/root-folders/{id}/unmapped` its child folders no library item
  occupies (the basis of "add existing media").

  Background:
    Given a daemon running in open mode

  @C34 @passing
  Scenario: a usable library root reports free space and writability
    Given the library root points at an existing writable directory
    When the client requests GET "/api/v1/root-folders"
    Then the response status is 200
    And the response body is a JSON array of length 1
    And the response field "0.id" is "library-root"
    And the response field "0.usable" is "true"
    And the response field "0.problem" is "null"
    And the response field "0.free_bytes" is present
    And the response field "0.total_bytes" is present

  @C34 @passing
  Scenario: a missing library root is reported with the reason
    Given the library root points at the missing path "/no/such/skadi/library"
    When the client requests GET "/api/v1/root-folders"
    Then the response field "0.exists" is "false"
    And the response field "0.usable" is "false"
    And the response field "0.problem" is "path does not exist"
    And the response field "0.free_bytes" is "null"

  @C34 @passing
  Scenario: unmapped folders are the root's children no domain occupies
    Given the library root points at an existing writable directory
    And the library root contains the folders "alpha,beta,gamma"
    And the "movies" library occupies the root folder "beta"
    And the "movies" domain is enabled
    When the client requests GET "/api/v1/root-folders/library-root/unmapped"
    Then the response body is a JSON array of length 2
    And the response field "0" contains "alpha"
    And the response field "1" contains "gamma"
