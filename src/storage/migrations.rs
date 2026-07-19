use rusqlite_migration::{M, Migrations};

pub const LATEST_SCHEMA_VERSION: i32 = 21;

pub static MIGRATIONS: std::sync::LazyLock<Migrations<'static>> = std::sync::LazyLock::new(|| {
    Migrations::new(vec![
        M::up(include_str!("schema_v1.sql")),
        M::up(include_str!("schema_v2.sql")),
        M::up(include_str!("schema_v3.sql")),
        M::up(include_str!("schema_v4.sql")),
        M::up(include_str!("schema_v5.sql")),
        M::up(include_str!("schema_v6.sql")),
        M::up(include_str!("schema_v7.sql")),
        M::up(include_str!("schema_v8.sql")),
        M::up(include_str!("schema_v9.sql")),
        M::up(include_str!("schema_v10.sql")),
        M::up(include_str!("schema_v11.sql")),
        M::up(include_str!("schema_v12.sql")),
        M::up(include_str!("schema_v13.sql")),
        M::up(include_str!("schema_v14.sql")),
        M::up(include_str!("schema_v15.sql")),
        M::up(include_str!("schema_v16.sql")),
        M::up(include_str!("schema_v17.sql")),
        M::up(include_str!("schema_v18.sql")),
        M::up(include_str!("schema_v19.sql")),
        M::up(include_str!("schema_v20.sql")),
        M::up(include_str!("schema_v21.sql")),
    ])
});
