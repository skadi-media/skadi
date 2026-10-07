Feature: C31 accessibility baseline
  Every control is reachable with Tab and works with Enter / Space, keyboard
  focus is always visible, icon-only controls have names, and axe-core finds
  no critical or serious problem on the main pages (SKADI-T-0700).
  Executable form: web-e2e/tests/a11y.spec.ts; the parts that mount on their
  own are also DOM tests in crates/skadi-web/tests/dom.rs.

  @C31 @passing
  Scenario: a keyboard-only pass through add, grab, watch and settings
    Given the harness daemon with the movies domain enabled
    When the operator uses only Tab, Shift+Tab, Enter, Space and "/"
    Then they add "The Matrix" from the Add page
    And open its tile and grab a release from the detail page
    And open the watch page and reach the player
    And open Settings, open the indexer form, and cancel it
    And every Tab stop on the way shows a focus ring

  @C31 @passing
  Scenario: axe finds no critical or serious problem on the main pages
    Given the harness daemon with movies and audiobooks enabled
    When axe-core runs on each main page, the Add results, a movie detail page with its releases, the watch page, select mode, the shortcut list, the indexer form and the open drawer at 375px
    Then no page has a critical or serious violation

  @C31 @passing
  Scenario: the open drawer locks the page scroll
    Given a 375px wide window
    When the operator opens the menu
    Then the page under the drawer does not scroll

  @C31 @gap
  Scenario: each page has one level-one heading
    Given the harness daemon with the movies domain enabled
    When axe-core runs on a main page
    Then it reports no page-has-heading-one or heading-order finding
