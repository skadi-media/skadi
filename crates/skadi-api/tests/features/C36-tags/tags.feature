Feature: C36 tags
  Sonarr/Radarr tags label library items and scope indexers, download clients,
  notifications and restrictions to those items (`/tag`, `/tag/detail`, bulk
  edit).

  Skadi has the **registry** (SKADI-T-0464): `tags` is a settings kind, and
  `/tag` is a Sonarr-compatible alias onto the same store. Membership — a tag
  field on movies/series/books — and provider scoping are SKADI-T-0550; until
  then a tag can be created and named but nothing is tagged with it.

  Background:
    Given a daemon running in open mode

  @C36 @passing @SKADI-T-0464
  Scenario: tags can be created and listed
    When the client sends POST "/api/v1/tag" with body:
      """
      { "label": "anime" }
      """
    Then the response status is 201
    When the client requests GET "/api/v1/tag"
    Then the response body is a JSON array of length 1

  @C36 @passing @SKADI-T-0464
  Scenario: tags are a settings kind, reachable by either path
    When the client requests GET "/api/v1/settings/tags"
    Then the response status is 200

  @C36 @passing @SKADI-T-0464
  Scenario: a label is normalised so two tags cannot look identical
    When the client sends POST "/api/v1/tag" with body:
      """
      { "label": "  Anime  " }
      """
    Then the response status is 201
    When the client requests GET "/api/v1/tag"
    Then the response body is a JSON array of length 1

  @C36 @passing @SKADI-T-0464
  Scenario: the same label cannot be registered twice
    When the client sends POST "/api/v1/tag" with body:
      """
      { "label": "anime" }
      """
    Then the response status is 201
    When the client sends POST "/api/v1/tag" with body:
      """
      { "label": "ANIME" }
      """
    Then the response status is 400

  @C36 @passing @SKADI-T-0464
  Scenario: an empty label is refused
    When the client sends POST "/api/v1/tag" with body:
      """
      { "label": "   " }
      """
    Then the response status is 400

  @C36 @passing @SKADI-T-0464
  Scenario: a comma in a label is refused because tag filters are comma-separated
    When the client sends POST "/api/v1/tag" with body:
      """
      { "label": "anime,4k" }
      """
    Then the response status is 400
