use sea_orm::entity::prelude::*;

/// Directed "waits for" edge between two work tasks of the same project
/// folder: `task_id` cannot be claimed until `depends_on_task_id` reached
/// `done`.
///
/// The gate is hard but never auto-failing: a failed/canceled/deleted
/// dependency leaves the dependent task in `todo` with a derived "blocked"
/// reason on the board, so the user (or the agent that built the split) decides
/// whether to fix the dependency or remove the edge.
#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "work_task_dependency")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub task_id: i32,
    pub depends_on_task_id: i32,
    pub created_at: DateTimeUtc,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::work_task::Entity",
        from = "Column::TaskId",
        to = "super::work_task::Column::Id"
    )]
    Task,
    #[sea_orm(
        belongs_to = "super::work_task::Entity",
        from = "Column::DependsOnTaskId",
        to = "super::work_task::Column::Id"
    )]
    DependsOn,
}

impl ActiveModelBehavior for ActiveModel {}
