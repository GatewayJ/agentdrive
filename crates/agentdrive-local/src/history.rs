use crate::{
    LocalBackend,
    sqlite::{read_many, sql_error},
};
use agentdrive_model::*;
use agentdrive_ports::RevisionHistoryScan;
use rusqlite::{params_from_iter, types::Value};

impl LocalBackend {
    pub(crate) async fn read_revision_metadata(
        &self,
        workspace: WorkspaceId,
        ids: Vec<RevisionId>,
    ) -> Result<Vec<Revision>> {
        self.db(move |connection| {
            let transaction = connection.transaction().map_err(sql_error)?;
            let mut result = Vec::new();
            for chunk in ids.chunks(200) {
                let placeholders = vec!["?"; chunk.len()].join(",");
                let mut args = vec![workspace.to_string()];
                args.extend(chunk.iter().map(ToString::to_string));
                result.extend(read_many::<Revision>(
                    &transaction,
                    &format!(
                        "SELECT data FROM revisions WHERE workspace=? AND id IN ({placeholders})"
                    ),
                    &args,
                )?);
            }
            Ok(result)
        })
        .await
    }

    pub(crate) async fn read_revision_history(
        &self,
        query: RevisionHistoryScan,
    ) -> Result<(Vec<Revision>, usize)> {
        self.db(move |connection| {
            let transaction = connection.transaction().map_err(sql_error)?;
            let mut condition = "workspace=? AND json_extract(CAST(data AS TEXT),'$.branch')=?".to_owned();
            let mut args = vec![Value::Text(query.workspace.to_string()), Value::Text(query.branch.to_string())];
            if !query.search.is_empty() {
                condition.push_str(" AND (instr(lower(id),lower(?))>0 OR instr(lower(COALESCE(json_extract(CAST(data AS TEXT),'$.message'),'')),lower(?))>0)");
                args.extend([Value::Text(query.search.clone()), Value::Text(query.search)]);
            }
            let total: i64 = transaction.query_row(
                &format!("SELECT count(*) FROM revisions WHERE {condition}"),
                params_from_iter(&args), |row| row.get(0),
            ).map_err(sql_error)?;
            if let Some((created, id)) = query.before {
                condition.push_str(" AND (json_extract(CAST(data AS TEXT),'$.created_ns')<? OR (json_extract(CAST(data AS TEXT),'$.created_ns')=? AND id>?))");
                args.extend([Value::Integer(created), Value::Integer(created), Value::Text(id.to_string())]);
            }
            args.push(Value::Integer(query.limit as i64));
            let mut statement = transaction.prepare_cached(&format!(
                "SELECT data FROM revisions WHERE {condition} ORDER BY json_extract(CAST(data AS TEXT),'$.created_ns') DESC,id ASC LIMIT ?"
            )).map_err(sql_error)?;
            let rows = statement.query_map(params_from_iter(&args), |row| row.get::<_, Vec<u8>>(0)).map_err(sql_error)?;
            let entries = rows.map(|row| decode(&row.map_err(sql_error)?)).collect::<Result<Vec<Revision>>>()?;
            Ok((entries, total as usize))
        }).await
    }
}
