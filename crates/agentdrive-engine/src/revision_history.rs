use crate::Runtime;
use agentdrive_model::*;
use agentdrive_ports::*;

pub(crate) async fn page(
    runtime: &Runtime,
    actor: &Principal,
    query: RevisionHistoryQuery,
) -> Result<RevisionHistoryPage> {
    let workspace = runtime
        .state
        .workspace(query.workspace)
        .await?
        .ok_or_else(|| Error::new(ErrorCode::NotFound, "workspace does not exist"))?;
    workspace.authorize(actor, Some(query.branch), Permission::Read)?;
    let local = runtime.state.branch(query.branch).await?;
    let known = match local {
        Some(branch) => branch.workspace == query.workspace,
        None => runtime
            .state
            .remote_refs(query.workspace)
            .await?
            .iter()
            .any(|reference| reference.branch == query.branch),
    };
    if !known {
        return Err(Error::new(
            ErrorCode::NotFound,
            "branch does not belong to this workspace",
        ));
    }
    let limit = query.limit.unwrap_or(50);
    if !(1..=200).contains(&limit) {
        return Err(Error::invalid(
            "history page size must be between 1 and 200",
        ));
    }
    let search = query.search.unwrap_or_default().trim().to_owned();
    if search.len() > 256 || search.chars().any(char::is_control) {
        return Err(Error::invalid(
            "history search must be at most 256 bytes without control characters",
        ));
    }
    let fingerprint = object_ref(
        ObjectKind::RecordIndex,
        &encode(&(actor, query.workspace, query.branch, &search))?,
    )
    .id;
    let before = query
        .cursor
        .map(|cursor| {
            if cursor.len() > 160 {
                return Err(Error::invalid("invalid history cursor"));
            }
            let mut fields = cursor.split(':');
            let version = fields
                .next()
                .ok_or_else(|| Error::invalid("invalid history cursor"))?;
            let created = fields
                .next()
                .and_then(|value| value.parse::<i64>().ok())
                .ok_or_else(|| Error::invalid("invalid history cursor"))?;
            let id = fields
                .next()
                .ok_or_else(|| Error::invalid("invalid history cursor"))?
                .parse::<RevisionId>()?;
            if version != fingerprint.to_string() || fields.next().is_some() {
                return Err(Error::invalid("history cursor does not match the query"));
            }
            Ok((created, id))
        })
        .transpose()?;
    let (mut entries, total) = runtime
        .state
        .revision_history(RevisionHistoryScan {
            workspace: query.workspace,
            branch: query.branch,
            search,
            before,
            limit: limit + 1,
        })
        .await?;
    let more = entries.len() > limit;
    entries.truncate(limit);
    let next = more.then(|| {
        let last = entries.last().expect("a nonempty bounded history page");
        format!("{fingerprint}:{}:{}", last.created_ns, last.id)
    });
    Ok(RevisionHistoryPage {
        entries,
        total,
        next,
    })
}
