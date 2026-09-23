Feature: Decision engine — reachability over quality (SKADI-T-0598)
  Without inbound peer connections a grab only completes if one of its seeders
  accepts connections, so among candidates at or above the resolution floor
  `decide` ranks by seeder band before quality. Below the floor stays a last
  resort; with the policy off the quality ladder decides as before.

  @C11 @passing @SKADI-T-0598
  Scenario: a well-seeded 720p beats a lone-seeder 1080p above the floor
    Given a wanted movie "Heat" (1995) with the standard profile
    And reachability is preferred over quality from 720p up
    And a candidate "Heat.1995.1080p.BluRay.x264-GRP" with 1 seeders
    And a candidate "Heat.1995.720p.WEB-DL.x264-GRP" with 40 seeders
    When the hunter decides
    Then it chooses "Heat.1995.720p.WEB-DL.x264-GRP"

  @C11 @passing @SKADI-T-0598
  Scenario: within one seeder band quality still decides
    Given a wanted movie "Heat" (1995) with the standard profile
    And reachability is preferred over quality from 720p up
    And a candidate "Heat.1995.1080p.BluRay.x264-GRP" with 12 seeders
    And a candidate "Heat.1995.720p.WEB-DL.x264-GRP" with 15 seeders
    When the hunter decides
    Then it chooses "Heat.1995.1080p.BluRay.x264-GRP"

  @C11 @passing @SKADI-T-0598
  Scenario: raising the floor to 1080p puts the 720p back below
    Given a wanted movie "Heat" (1995) with the standard profile
    And reachability is preferred over quality from 1080p up
    And a candidate "Heat.1995.1080p.BluRay.x264-GRP" with 1 seeders
    And a candidate "Heat.1995.720p.WEB-DL.x264-GRP" with 40 seeders
    When the hunter decides
    Then it chooses "Heat.1995.1080p.BluRay.x264-GRP"

  @C11 @passing @SKADI-T-0598
  Scenario: with the policy off the quality ladder decides
    Given a wanted movie "Heat" (1995) with the standard profile
    And quality is preferred over reachability
    And a candidate "Heat.1995.1080p.BluRay.x264-GRP" with 1 seeders
    And a candidate "Heat.1995.720p.WEB-DL.x264-GRP" with 40 seeders
    When the hunter decides
    Then it chooses "Heat.1995.1080p.BluRay.x264-GRP"
