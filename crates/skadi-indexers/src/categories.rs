//! The standard Newznab category catalog (SKADI-T-0206).
//!
//! Indexer config takes raw Newznab category numbers (`2000`, `3030`). This is the
//! named tree the picker UI renders so an operator selects "Movies" / "Audio/
//! Audiobook" instead of memorising numbers. The Newznab taxonomy is a published
//! standard (the names/ids here match a live Prowlarr 2.4's `/api/v1/indexer/
//! categories`), so this is a static catalog — no live query needed. Which of these
//! a *specific* indexer actually serves comes from
//! [`Prowlarr::capabilities`](crate::Prowlarr) (SKADI-T-0205).

use serde::Serialize;

/// One Newznab category (a node in the standard tree).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct CategoryInfo {
    pub id: u32,
    pub name: &'static str,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub sub_categories: Vec<CategoryInfo>,
}

fn cat(id: u32, name: &'static str, sub_categories: Vec<CategoryInfo>) -> CategoryInfo {
    CategoryInfo {
        id,
        name,
        sub_categories,
    }
}

fn leaf(id: u32, name: &'static str) -> CategoryInfo {
    cat(id, name, Vec::new())
}

/// The standard Newznab top-level categories, with the skadi-relevant subtrees
/// (Movies, Audio incl. Audiobook, TV, Books) expanded; the rest are top-level
/// only. Stable ids — they're the published Newznab numbers.
#[must_use]
pub fn standard_categories() -> Vec<CategoryInfo> {
    vec![
        leaf(1000, "Console"),
        cat(
            2000,
            "Movies",
            vec![
                leaf(2010, "Movies/Foreign"),
                leaf(2020, "Movies/Other"),
                leaf(2030, "Movies/SD"),
                leaf(2040, "Movies/HD"),
                leaf(2045, "Movies/UHD"),
                leaf(2050, "Movies/BluRay"),
                leaf(2060, "Movies/3D"),
                leaf(2070, "Movies/DVD"),
                leaf(2080, "Movies/WEB-DL"),
            ],
        ),
        cat(
            3000,
            "Audio",
            vec![
                leaf(3010, "Audio/MP3"),
                leaf(3020, "Audio/Video"),
                leaf(3030, "Audio/Audiobook"),
                leaf(3040, "Audio/Lossless"),
                leaf(3050, "Audio/Other"),
                leaf(3060, "Audio/Foreign"),
            ],
        ),
        leaf(4000, "PC"),
        cat(
            5000,
            "TV",
            vec![
                leaf(5010, "TV/WEB-DL"),
                leaf(5020, "TV/Foreign"),
                leaf(5030, "TV/SD"),
                leaf(5040, "TV/HD"),
                leaf(5045, "TV/UHD"),
                leaf(5050, "TV/Other"),
                leaf(5060, "TV/Sport"),
                leaf(5070, "TV/Anime"),
            ],
        ),
        leaf(6000, "XXX"),
        cat(
            7000,
            "Books",
            vec![
                leaf(7010, "Books/Mags"),
                leaf(7020, "Books/EBook"),
                leaf(7030, "Books/Comics"),
                leaf(7040, "Books/Technical"),
                leaf(7050, "Books/Other"),
                leaf(7060, "Books/Foreign"),
            ],
        ),
        leaf(8000, "Other"),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_has_the_skadi_relevant_categories() {
        let cats = standard_categories();
        let find = |id: u32| cats.iter().find(|c| c.id == id);

        // Movies (2000) with the HD subcategory.
        let movies = find(2000).expect("movies");
        assert_eq!(movies.name, "Movies");
        assert!(
            movies
                .sub_categories
                .iter()
                .any(|s| s.id == 2040 && s.name == "Movies/HD")
        );

        // Audiobook (3030) under Audio (3000) — the audiobooks domain's category.
        let audio = find(3000).expect("audio");
        assert!(
            audio
                .sub_categories
                .iter()
                .any(|s| s.id == 3030 && s.name == "Audio/Audiobook"),
            "audiobook category present"
        );

        // Top-level standard set is complete.
        for id in [1000, 2000, 3000, 4000, 5000, 6000, 7000, 8000] {
            assert!(find(id).is_some(), "missing top-level category {id}");
        }
    }

    #[test]
    fn serializes_omitting_empty_subcategories() {
        let json = serde_json::to_value(standard_categories()).unwrap();
        // A leaf (Console) has no `sub_categories` key; Movies does.
        let console = json
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["id"] == 1000)
            .unwrap();
        assert!(console.get("sub_categories").is_none());
        let movies = json
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["id"] == 2000)
            .unwrap();
        assert!(movies["sub_categories"].as_array().unwrap().len() >= 8);
    }
}
