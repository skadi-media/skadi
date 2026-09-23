Feature: C31 phone pairing card
  Listen → "Set up the Android app" renders /pair/app as a QR carrying host
  + token, and /pair/apk as an install link when a build is published.

  @C31 @passing
  Scenario: the pairing card renders a QR that encodes skadi://pair with the token
    Given the harness daemon with the audiobooks domain enabled
    When the operator opens "/listen"
    Then the pairing card shows an SVG QR
    And the pairing URL shown starts with "skadi://pair?host="

  @C31 @passing
  Scenario: no published APK means no install link, not an error
    Given the harness daemon with the audiobooks domain enabled
    When the operator opens "/listen"
    Then the pairing card does not offer an APK download
    And the page does not show an error dialog
