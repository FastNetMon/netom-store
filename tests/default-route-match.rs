#[cfg(test)]
mod tests {
    use inetnum::addr::Prefix;
    use rotonda_store::{
        epoch,
        match_options::{IncludeHistory, MatchOptions, MatchType},
        prefix_record::{Record, RouteStatus},
        rib::{config::Config, StarCastRib},
        test_types::NoMeta,
    };

    rotonda_store::all_strategies![
        test_default_route;
        test_default_route_match_ipv4;
        NoMeta
    ];

    fn opts(match_type: MatchType) -> MatchOptions {
        MatchOptions {
            match_type,
            include_withdrawn: false,
            include_less_specifics: false,
            include_more_specifics: false,
            mui: None,
            include_history: IncludeHistory::None,
        }
    }

    fn test_default_route_match_ipv4<C: Config>(
        trie: StarCastRib<NoMeta, C>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let default =
            Prefix::new(std::net::Ipv4Addr::new(0, 0, 0, 0).into(), 0)?;
        let specific =
            Prefix::new(std::net::Ipv4Addr::new(192, 0, 2, 0).into(), 24)?;

        for pfx in [&default, &specific] {
            trie.insert(
                pfx,
                Record::new(0, 0, RouteStatus::Active, NoMeta::Empty),
                None,
            )?;
        }

        let guard = &epoch::pin();

        // Exact match on the default route itself.
        let res =
            trie.match_prefix(&default, &opts(MatchType::ExactMatch), guard)?;
        assert_eq!(res.prefix, Some(default));

        // Longest match for an address only the default route covers.
        let uncovered =
            Prefix::new(std::net::Ipv4Addr::new(8, 8, 8, 8).into(), 32)?;
        let res = trie.match_prefix(
            &uncovered,
            &opts(MatchType::LongestMatch),
            guard,
        )?;
        assert_eq!(res.prefix, Some(default));

        // A more specific route still wins over the default route.
        let covered =
            Prefix::new(std::net::Ipv4Addr::new(192, 0, 2, 1).into(), 32)?;
        let res = trie.match_prefix(
            &covered,
            &opts(MatchType::LongestMatch),
            guard,
        )?;
        assert_eq!(res.prefix, Some(specific));

        Ok(())
    }
}
