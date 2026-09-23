Feature: Composing a full library path (C21)
  Root + folder template (with `/` for nesting) + file template + extension, each
  component sanitized, empty components dropped. Mirrors Radarr "Movie Folder
  Format" / "Standard Movie Format" and Sonarr's series/season folder formats.

  @C21 @passing
  Scenario: the default movie layout reproduces the operator's kebab, underscore, id-tagged scheme
    Given the movie token set the domain supplies
    When folder template "{TitleKebab} ({Year}) {TmdbTag} {ImdbTag}/{EditionKebab}" and file template "{TitleKebab} ({Year})" build a path under "/library/movie" with extension ".mkv"
    Then the path is "/library/movie/the-matrix_(1999)_{tmdb-603}_{imdb-tt0133093}/theatrical/the-matrix_(1999).mkv"

  @C21 @passing
  Scenario: an edition gets its own subfolder and filename suffix
    Given the token "Title" is "Blade Runner"
    And the token "Year" is "1982"
    And the token "TmdbTag" is "{tmdb-78}"
    And the token "EditionFolder" is "Final Cut"
    And the token "EditionSuffix" is "-Final Cut"
    When folder template "{Title} ({Year}) {TmdbTag}/{EditionFolder}" and file template "{Title} ({Year}){EditionSuffix}" build a path under "/movies" with extension ".mp4"
    Then the path is "/movies/Blade_Runner_(1982)_{tmdb-78}/Final_Cut/Blade_Runner_(1982)-Final_Cut.mp4"

  @C21 @passing
  Scenario: missing year and id tags collapse away without leaving empty parens
    Given the token "Title" is "Blade Runner"
    And the token "Year" is empty
    And the token "TmdbTag" is empty
    And the token "EditionFolder" is "Theatrical"
    When folder template "{Title} ({Year}) {TmdbTag}/{EditionFolder}" and file template "{Title} ({Year})" build a path under "/movies" with extension ".mkv"
    Then the path is "/movies/Blade_Runner/Theatrical/Blade_Runner.mkv"

  @C21 @passing
  Scenario: a folder level whose tokens are all empty is dropped rather than left blank
    Given the token "Author" is "Stephen King"
    And the token "Series" is empty
    And the token "Title" is "It"
    When folder template "{Author}/{Series}/{Title}" and file template "{Title}" build a path under "/audiobook" with extension ".m4b"
    Then the path is "/audiobook/Stephen_King/It/It.m4b"

  @C21 @passing
  Scenario: the series layout nests a season folder under the series folder
    Given the episode token set the domain supplies
    When folder template "{SeriesTitleKebab} ({Year}) {TmdbTag}/{SeasonFolder}" and file template "{SeriesTitleKebab} - {Episode}{EpisodeTitlePart}" build a path under "/library/television" with extension ".mkv"
    Then the path is "/library/television/the-wire_(2002)_{tmdb-1438}/Season_03/the-wire_-_S03E05_-_straight-and-true.mkv"

  @C21 @passing
  Scenario: the space-preserving mode renders a Plex-style human layout
    Given the movie token set the domain supplies
    And the space replacement is " "
    When folder template "{Title} ({Year}) {TmdbTag}" and file template "{Title} ({Year})" build a path under "/library/movie" with extension ".mkv"
    Then the path is "/library/movie/The Matrix (1999) {tmdb-603}/The Matrix (1999).mkv"

  @C21 @passing
  Scenario: an absolute-looking title is confined to a single component under the root
    Given the token "Title" is "/etc/passwd"
    When folder template "{Title}" and file template "{Title}" build a path under "/library/movie" with extension ".mkv"
    Then the path is "/library/movie/etc_passwd/etc_passwd.mkv"
    And the path stays under the root

  @C21 @passing
  Scenario: a curly-quoted title renders to a unicode path
    Given the token "Title" is "‘Salem’s Lot"
    And the token "Year" is "2024"
    When folder template "{Title} ({Year})" and file template "{Title} ({Year})" build a path under "/library/movie" with extension ".mkv"
    Then the path is "/library/movie/‘Salem’s_Lot_(2024)/‘Salem’s_Lot_(2024).mkv"
