Feature: Cardigann engine — login flows, grab-time download resolution, definition loading
  login.rs (form/post/cookie login + `login.test`), download.rs (`download.infohash`
  blocks → magnet), lib.rs / catalog.rs (lenient parsing, warn-skip batch load).

  @C15 @passing
  Scenario: a form login posts the rendered inputs to the form action and the test page confirms the session
    Given the definition:
      """
      id: priv
      name: Priv
      type: private
      links: [https://priv.test/]
      settings:
        - {name: username, type: text, label: Username}
        - {name: password, type: password, label: Password}
      login:
        path: login.php
        method: form
        form: form#login
        inputs:
          username: "{{ .Config.username }}"
          password: "{{ .Config.password }}"
        test:
          path: account.php
          selector: a.logout
      search:
        rows: {selector: tr}
      """
    And the config override "username" is "alice"
    And the config override "password" is "hunter2"
    And the site base URL "https://priv.test"
    And the tracker answers URLs containing "/login.php" with HTTP 200 and body:
      """
      <form id="login" action="/takelogin.php"><input type="hidden" name="csrf" value="tok"></form>
      """
    And the tracker answers URLs containing "takelogin.php" with HTTP 200
    And the tracker answers URLs containing "account.php" with HTTP 200 and body:
      """
      <a class="logout">out</a>
      """
    When the engine logs in
    Then the login outcome is "ok"
    And the tracker received a POST whose body contains "username=alice"
    And the tracker received a POST whose body contains "password=hunter2"
    And the tracker received a POST whose body contains "csrf=tok"
    And the definition needs a login

  @C15 @passing
  Scenario: a login whose test selector is absent is a failed login with the tracker's error text
    Given the definition:
      """
      id: priv
      name: Priv
      type: private
      links: [https://priv.test/]
      login:
        path: login.php
        method: form
        form: form#login
        inputs:
          username: "{{ .Config.username }}"
          password: "{{ .Config.password }}"
        error:
          - selector: div.err
            message: {text: "Invalid password"}
        test:
          path: account.php
          selector: a.logout
      search:
        rows: {selector: tr}
      """
    And the site base URL "https://priv.test"
    And the tracker answers URLs containing "/login.php" with HTTP 200 and body:
      """
      <form id="login" action="/takelogin.php"></form>
      """
    And the tracker answers URLs containing "takelogin.php" with HTTP 200 and body:
      """
      <div class="err">Invalid password</div>
      """
    And the tracker answers URLs containing "account.php" with HTTP 200 and body:
      """
      <a class="login">in</a>
      """
    When the engine logs in
    Then the login outcome is "failed"
    And the login failed with a reason containing "Invalid password"

  @C15 @passing
  Scenario: a public definition needs no login
    Given the definition:
      """
      id: pub
      name: Pub
      type: public
      search:
        rows: {selector: tr}
      """
    When the engine logs in
    Then the login outcome is "not required"
    And the definition does not need a login

  @C15 @passing
  Scenario: a download.infohash block scrapes the hash off a detail page and builds a magnet
    Given the definition:
      """
      id: abb
      name: ABB
      type: public
      search:
        rows: {selector: tr}
      download:
        infohash:
          hash:
            selector: 'td:contains("Info Hash:") ~ td'
            filters:
              - name: regexp
                args: "([A-Fa-f0-9]{40})"
      """
    And the tracker answers URLs containing "/abss/x" with HTTP 200 and body:
      """
      <table><tr><td>Info Hash:</td><td>0123456789ABCDEF0123456789ABCDEF01234567</td></tr></table>
      """
    When the engine resolves the download URL "https://abb.test/abss/x"
    Then the resolved magnet is "magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234567"

  @C15 @passing
  Scenario: a detail page without a valid 40-char hash is a resolution error, never a silent pass-through
    Given the definition:
      """
      id: abb
      name: ABB
      type: public
      search:
        rows: {selector: tr}
      download:
        infohash:
          hash:
            selector: 'td:contains("Info Hash:") ~ td'
      """
    And the tracker answers URLs containing "/abss/x" with HTTP 200 and body:
      """
      <table><tr><td>Info Hash:</td><td>not-a-hash</td></tr></table>
      """
    When the engine resolves the download URL "https://abb.test/abss/x"
    Then the resolution fails with an error containing "did not yield a 40-char info-hash"

  @C15 @passing
  Scenario: a definition without a download block leaves the fetch alone
    Given the definition:
      """
      id: plain
      name: Plain
      type: public
      search:
        rows: {selector: tr}
      """
    When the engine resolves the download URL "https://plain.test/dl/1.torrent"
    Then no download resolution applies
    And the tracker received 0 requests

  @C15 @passing
  Scenario: a malformed definition is rejected with its id recovered, and a batch load skips only the bad one
    Given the definition:
      """
      id: badtracker
      name: Bad
      caps: 42
      search: {}
      """
    Then the definition is rejected for id "badtracker" with a reason mentioning "caps"
    And a batch of one good and one malformed definition loads the good one and reports the bad one

  @C15 @passing
  Scenario: the caps.categories map form is parsed alongside categorymappings
    Given the definition:
      """
      id: eztvlike
      name: EZTV
      type: public
      caps:
        categories:
          1: TV
          2: TV/HD
        modes: {tv-search: [q, season, ep]}
      search:
        rows: {selector: tr}
      """
    Then the definition parses
    And the definition maps tracker category "2" to "TV/HD"

  @C15 @passing
  Scenario: the catalog loads a directory recursively, warn-skipping malformed files and ignoring non-YAML
    Given a definitions directory holding two valid definitions and one malformed file
    Then the catalog lists "alpha,beta" and reports 1 load error
    And the catalog entry "beta" is "private" and offers the setting "username"
