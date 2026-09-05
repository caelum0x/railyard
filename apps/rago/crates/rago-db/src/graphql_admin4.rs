//! Repository extensions for the GraphQL dashboard admin CRUD surface — slice 4:
//! catalog v2 (products / rate cards / pricing units / commitments) + auth/RBAC
//! (users / memberships / roles / invites / password resets) + billing-entity
//! taxes. Password material is stored only as an argon2 digest (the hashing
//! happens in `rago-graphql::auth`); this layer persists the digest verbatim.

use rago_core::models::Tax;
use serde_json::Value;
use sqlx::Row;
use uuid::Uuid;

use crate::error::Result;
use crate::mapping;
use crate::repo::Db;

// ---- row structs -----------------------------------------------------------

/// A user `(id, email, password_digest)`.
pub type UserRow = (Uuid, String, Option<String>);

#[derive(Debug, Clone)]
pub struct MembershipRow {
    pub id: Uuid,
    pub user_id: Uuid,
    pub role: String,
    pub roles: Value,
    pub status: String,
}
fn membership_row(r: &sqlx::postgres::PgRow) -> MembershipRow {
    MembershipRow { id: r.get("id"), user_id: r.get("user_id"), role: r.get("role"), roles: r.get("roles"), status: r.get("status") }
}

#[derive(Debug, Clone)]
pub struct RoleRow {
    pub id: Uuid,
    pub code: String,
    pub name: String,
    pub description: Option<String>,
    pub permissions: Value,
}
fn role_row(r: &sqlx::postgres::PgRow) -> RoleRow {
    RoleRow { id: r.get("id"), code: r.get("code"), name: r.get("name"), description: r.get("description"), permissions: r.get("permissions") }
}

#[derive(Debug, Clone)]
pub struct InviteRow {
    pub id: Uuid,
    pub organization_id: Uuid,
    pub email: String,
    pub token: String,
    pub roles: Value,
    pub status: String,
}
fn invite_row(r: &sqlx::postgres::PgRow) -> InviteRow {
    InviteRow {
        id: r.get("id"),
        organization_id: r.get("organization_id"),
        email: r.get("email"),
        token: r.get("token"),
        roles: r.get("roles"),
        status: r.get("status"),
    }
}

#[derive(Debug, Clone)]
pub struct ProductRow {
    pub id: Uuid,
    pub code: String,
    pub name: String,
    pub description: Option<String>,
    pub invoice_display_name: Option<String>,
    pub product_type: Option<String>,
    pub billable_metric_id: Option<Uuid>,
    pub product_category_id: Option<Uuid>,
}
fn product_row(r: &sqlx::postgres::PgRow) -> ProductRow {
    ProductRow {
        id: r.get("id"),
        code: r.get("code"),
        name: r.get("name"),
        description: r.get("description"),
        invoice_display_name: r.get("invoice_display_name"),
        product_type: r.get("product_type"),
        billable_metric_id: r.get("billable_metric_id"),
        product_category_id: r.get("product_category_id"),
    }
}

/// A product category `(id, code, name, description, invoice_display_name)`.
pub type ProductCategoryRow = (Uuid, String, String, Option<String>, Option<String>);

#[derive(Debug, Clone)]
pub struct ProductFilterRow {
    pub id: Uuid,
    pub product_id: Uuid,
    pub code: String,
    pub name: String,
    pub invoice_display_name: Option<String>,
    pub values: Value,
}
fn product_filter_row(r: &sqlx::postgres::PgRow) -> ProductFilterRow {
    ProductFilterRow {
        id: r.get("id"),
        product_id: r.get("product_id"),
        code: r.get("code"),
        name: r.get("name"),
        invoice_display_name: r.get("invoice_display_name"),
        values: r.get("values"),
    }
}

#[derive(Debug, Clone)]
pub struct RateCardRow {
    pub id: Uuid,
    pub product_id: Uuid,
    pub code: String,
    pub name: String,
    pub description: Option<String>,
    pub currency: String,
    pub billing_timing: Option<String>,
    pub proration: bool,
    pub display_on_invoice: bool,
    pub wallet_targetable: bool,
}
fn rate_card_row(r: &sqlx::postgres::PgRow) -> RateCardRow {
    RateCardRow {
        id: r.get("id"),
        product_id: r.get("product_id"),
        code: r.get("code"),
        name: r.get("name"),
        description: r.get("description"),
        currency: r.get("currency"),
        billing_timing: r.get("billing_timing"),
        proration: r.get("proration"),
        display_on_invoice: r.get("display_on_invoice"),
        wallet_targetable: r.get("wallet_targetable"),
    }
}

/// A rate-card rate `(id, rate_card_id, charge_model, properties)`.
pub type RateCardRateRow = (Uuid, Uuid, i32, Value);

/// A plan-applied rate card `(id, plan_id, rate_card_id)`.
pub type PlanAppliedRateCardRow = (Uuid, Uuid, Uuid);

#[derive(Debug, Clone)]
pub struct RatePhaseRow {
    pub id: Uuid,
    pub plan_applied_rate_card_id: Uuid,
    pub code: String,
    pub name: Option<String>,
    pub position: Option<i32>,
    pub billing_interval_cycle_count: Option<i32>,
}
fn rate_phase_row(r: &sqlx::postgres::PgRow) -> RatePhaseRow {
    RatePhaseRow {
        id: r.get("id"),
        plan_applied_rate_card_id: r.get("plan_applied_rate_card_id"),
        code: r.get("code"),
        name: r.get("name"),
        position: r.get("position"),
        billing_interval_cycle_count: r.get("billing_interval_cycle_count"),
    }
}

/// A pricing unit `(id, code, name, short_name, description)`.
pub type PricingUnitRow = (Uuid, String, String, String, Option<String>);

/// A commitment `(id, plan_id, commitment_type, amount_cents, invoice_display_name)`.
pub type CommitmentRow = (Uuid, Option<Uuid>, String, i64, Option<String>);

impl Db {
    // ---- users ----------------------------------------------------------

    pub async fn insert_user(&self, id: Uuid, email: &str, password_digest: Option<&str>) -> Result<()> {
        sqlx::query("INSERT INTO users (id, email, password_digest, created_at, updated_at) VALUES ($1,$2,$3,now(),now())")
            .bind(id)
            .bind(email)
            .bind(password_digest)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn find_user_by_email(&self, email: &str) -> Result<Option<UserRow>> {
        let row = sqlx::query("SELECT id, email, password_digest FROM users WHERE lower(email) = lower($1)")
            .bind(email)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.map(|r| (r.get("id"), r.get("email"), r.get("password_digest"))))
    }

    pub async fn find_user(&self, id: Uuid) -> Result<Option<(Uuid, String)>> {
        let row = sqlx::query("SELECT id, email FROM users WHERE id = $1").bind(id).fetch_optional(&self.pool).await?;
        Ok(row.map(|r| (r.get("id"), r.get("email"))))
    }

    pub async fn update_user_password(&self, user_id: Uuid, digest: &str) -> Result<()> {
        sqlx::query("UPDATE users SET password_digest = $2, updated_at = now() WHERE id = $1").bind(user_id).bind(digest).execute(&self.pool).await?;
        Ok(())
    }

    // ---- memberships ----------------------------------------------------

    pub async fn insert_membership(&self, id: Uuid, org: Uuid, user_id: Uuid, role: &str, roles: &Value) -> Result<MembershipRow> {
        sqlx::query("INSERT INTO memberships (id, organization_id, user_id, role, roles, created_at, updated_at) VALUES ($1,$2,$3,$4,$5,now(),now())")
            .bind(id)
            .bind(org)
            .bind(user_id)
            .bind(role)
            .bind(roles)
            .execute(&self.pool)
            .await?;
        Ok(self.find_membership(id).await?.expect("just inserted"))
    }

    pub async fn find_membership(&self, id: Uuid) -> Result<Option<MembershipRow>> {
        let row = sqlx::query("SELECT * FROM memberships WHERE id = $1").bind(id).fetch_optional(&self.pool).await?;
        Ok(row.as_ref().map(membership_row))
    }

    pub async fn membership_for(&self, org: Uuid, user_id: Uuid) -> Result<Option<MembershipRow>> {
        let row = sqlx::query("SELECT * FROM memberships WHERE organization_id = $1 AND user_id = $2 AND status = 'active'")
            .bind(org)
            .bind(user_id)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.as_ref().map(membership_row))
    }

    pub async fn list_memberships(&self, org: Uuid) -> Result<Vec<MembershipRow>> {
        let rows = sqlx::query("SELECT * FROM memberships WHERE organization_id = $1 ORDER BY created_at").bind(org).fetch_all(&self.pool).await?;
        Ok(rows.iter().map(membership_row).collect())
    }

    pub async fn update_membership_roles(&self, id: Uuid, roles: &Value) -> Result<Option<MembershipRow>> {
        sqlx::query("UPDATE memberships SET roles = $2, updated_at = now() WHERE id = $1").bind(id).bind(roles).execute(&self.pool).await?;
        self.find_membership(id).await
    }

    pub async fn revoke_membership(&self, id: Uuid) -> Result<Option<MembershipRow>> {
        sqlx::query("UPDATE memberships SET status = 'revoked', revoked_at = now(), updated_at = now() WHERE id = $1").bind(id).execute(&self.pool).await?;
        self.find_membership(id).await
    }

    // ---- roles ----------------------------------------------------------

    pub async fn insert_role(&self, id: Uuid, org: Uuid, code: &str, name: &str, description: Option<&str>, permissions: &Value) -> Result<RoleRow> {
        sqlx::query("INSERT INTO roles (id, organization_id, code, name, description, permissions, created_at, updated_at) VALUES ($1,$2,$3,$4,$5,$6,now(),now())")
            .bind(id)
            .bind(org)
            .bind(code)
            .bind(name)
            .bind(description)
            .bind(permissions)
            .execute(&self.pool)
            .await?;
        Ok(self.find_role(id).await?.expect("just inserted"))
    }

    pub async fn update_role(&self, id: Uuid, name: Option<&str>, description: Option<&str>, permissions: Option<&Value>) -> Result<Option<RoleRow>> {
        sqlx::query("UPDATE roles SET name = COALESCE($2, name), description = COALESCE($3, description), permissions = COALESCE($4, permissions), updated_at = now() WHERE id = $1")
            .bind(id)
            .bind(name)
            .bind(description)
            .bind(permissions)
            .execute(&self.pool)
            .await?;
        self.find_role(id).await
    }

    pub async fn soft_delete_role(&self, id: Uuid) -> Result<()> {
        sqlx::query("UPDATE roles SET deleted_at = now(), updated_at = now() WHERE id = $1").bind(id).execute(&self.pool).await?;
        Ok(())
    }

    pub async fn find_role(&self, id: Uuid) -> Result<Option<RoleRow>> {
        let row = sqlx::query("SELECT * FROM roles WHERE id = $1 AND deleted_at IS NULL").bind(id).fetch_optional(&self.pool).await?;
        Ok(row.as_ref().map(role_row))
    }

    pub async fn list_roles(&self, org: Uuid) -> Result<Vec<RoleRow>> {
        let rows = sqlx::query("SELECT * FROM roles WHERE organization_id = $1 AND deleted_at IS NULL ORDER BY created_at").bind(org).fetch_all(&self.pool).await?;
        Ok(rows.iter().map(role_row).collect())
    }

    // ---- invites --------------------------------------------------------

    pub async fn insert_invite(&self, id: Uuid, org: Uuid, email: &str, token: &str, roles: &Value) -> Result<InviteRow> {
        sqlx::query("INSERT INTO invites (id, organization_id, email, token, roles, created_at, updated_at) VALUES ($1,$2,$3,$4,$5,now(),now())")
            .bind(id)
            .bind(org)
            .bind(email)
            .bind(token)
            .bind(roles)
            .execute(&self.pool)
            .await?;
        Ok(self.find_invite(id).await?.expect("just inserted"))
    }

    pub async fn find_invite(&self, id: Uuid) -> Result<Option<InviteRow>> {
        let row = sqlx::query("SELECT * FROM invites WHERE id = $1").bind(id).fetch_optional(&self.pool).await?;
        Ok(row.as_ref().map(invite_row))
    }

    pub async fn find_invite_by_token(&self, token: &str) -> Result<Option<InviteRow>> {
        let row = sqlx::query("SELECT * FROM invites WHERE token = $1").bind(token).fetch_optional(&self.pool).await?;
        Ok(row.as_ref().map(invite_row))
    }

    pub async fn list_invites(&self, org: Uuid) -> Result<Vec<InviteRow>> {
        let rows = sqlx::query("SELECT * FROM invites WHERE organization_id = $1 ORDER BY created_at DESC").bind(org).fetch_all(&self.pool).await?;
        Ok(rows.iter().map(invite_row).collect())
    }

    pub async fn update_invite_roles(&self, id: Uuid, roles: &Value) -> Result<Option<InviteRow>> {
        sqlx::query("UPDATE invites SET roles = $2, updated_at = now() WHERE id = $1").bind(id).bind(roles).execute(&self.pool).await?;
        self.find_invite(id).await
    }

    pub async fn revoke_invite(&self, id: Uuid) -> Result<Option<InviteRow>> {
        sqlx::query("UPDATE invites SET status = 'revoked', revoked_at = now(), updated_at = now() WHERE id = $1").bind(id).execute(&self.pool).await?;
        self.find_invite(id).await
    }

    pub async fn mark_invite_accepted(&self, id: Uuid) -> Result<()> {
        sqlx::query("UPDATE invites SET status = 'accepted', accepted_at = now(), updated_at = now() WHERE id = $1").bind(id).execute(&self.pool).await?;
        Ok(())
    }

    // ---- password resets ------------------------------------------------

    pub async fn insert_password_reset(&self, id: Uuid, user_id: Uuid, token: &str) -> Result<()> {
        sqlx::query("INSERT INTO password_resets (id, user_id, token, expire_at, created_at, updated_at) VALUES ($1,$2,$3, now() + interval '1 day', now(), now())")
            .bind(id)
            .bind(user_id)
            .bind(token)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn find_password_reset_by_token(&self, token: &str) -> Result<Option<(Uuid, Uuid)>> {
        let row = sqlx::query("SELECT id, user_id FROM password_resets WHERE token = $1 AND (expire_at IS NULL OR expire_at > now())")
            .bind(token)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.map(|r| (r.get("id"), r.get("user_id"))))
    }

    pub async fn delete_password_reset(&self, id: Uuid) -> Result<()> {
        sqlx::query("DELETE FROM password_resets WHERE id = $1").bind(id).execute(&self.pool).await?;
        Ok(())
    }

    // ---- billing-entity taxes -------------------------------------------

    pub async fn find_tax_by_code(&self, org: Uuid, code: &str) -> Result<Option<Tax>> {
        let row = sqlx::query("SELECT * FROM taxes WHERE organization_id = $1 AND code = $2").bind(org).bind(code).fetch_optional(&self.pool).await?;
        row.as_ref().map(mapping::tax).transpose()
    }

    pub async fn apply_billing_entity_tax(&self, org: Uuid, billing_entity_id: Uuid, tax_id: Uuid) -> Result<()> {
        sqlx::query("INSERT INTO billing_entity_taxes (id, organization_id, billing_entity_id, tax_id, created_at) VALUES (gen_random_uuid(),$1,$2,$3,now()) ON CONFLICT (billing_entity_id, tax_id) DO NOTHING")
            .bind(org)
            .bind(billing_entity_id)
            .bind(tax_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn remove_billing_entity_tax(&self, billing_entity_id: Uuid, tax_id: Uuid) -> Result<()> {
        sqlx::query("DELETE FROM billing_entity_taxes WHERE billing_entity_id = $1 AND tax_id = $2").bind(billing_entity_id).bind(tax_id).execute(&self.pool).await?;
        Ok(())
    }

    pub async fn billing_entity_taxes(&self, billing_entity_id: Uuid) -> Result<Vec<Tax>> {
        let rows = sqlx::query("SELECT t.* FROM taxes t JOIN billing_entity_taxes b ON b.tax_id = t.id WHERE b.billing_entity_id = $1 ORDER BY t.code")
            .bind(billing_entity_id)
            .fetch_all(&self.pool)
            .await?;
        rows.iter().map(mapping::tax).collect()
    }

    // ---- api keys (rotate / destroy) ------------------------------------

    /// Rotate an API key: expire the old one and mint a replacement (returns the
    /// new key's `(id, value, name)`).
    pub async fn rotate_api_key(&self, org: Uuid, old_id: Uuid, name: Option<&str>) -> Result<Option<(Uuid, String, Option<String>)>> {
        let exists = sqlx::query("SELECT name FROM api_keys WHERE id = $1 AND organization_id = $2")
            .bind(old_id)
            .bind(org)
            .fetch_optional(&self.pool)
            .await?;
        let Some(old) = exists else { return Ok(None) };
        let carried_name = name.map(|s| s.to_string()).or_else(|| old.get::<Option<String>, _>("name"));
        sqlx::query("UPDATE api_keys SET expires_at = now(), updated_at = now() WHERE id = $1").bind(old_id).execute(&self.pool).await?;
        let id = Uuid::new_v4();
        let value = format!("rago_{}", Uuid::new_v4().simple());
        sqlx::query("INSERT INTO api_keys (id, organization_id, value, name, permissions, created_at, updated_at) VALUES ($1,$2,$3,$4,'{}',now(),now())")
            .bind(id)
            .bind(org)
            .bind(&value)
            .bind(carried_name.as_deref())
            .execute(&self.pool)
            .await?;
        Ok(Some((id, value, carried_name)))
    }

    pub async fn destroy_api_key(&self, org: Uuid, id: Uuid) -> Result<bool> {
        let r = sqlx::query("UPDATE api_keys SET expires_at = now(), updated_at = now() WHERE id = $1 AND organization_id = $2")
            .bind(id)
            .bind(org)
            .execute(&self.pool)
            .await?;
        Ok(r.rows_affected() > 0)
    }

    // ---- catalog: product categories ------------------------------------

    pub async fn insert_product_category(&self, id: Uuid, org: Uuid, code: &str, name: &str, description: Option<&str>, invoice_display_name: Option<&str>) -> Result<ProductCategoryRow> {
        sqlx::query("INSERT INTO product_categories (id, organization_id, code, name, description, invoice_display_name, created_at, updated_at) VALUES ($1,$2,$3,$4,$5,$6,now(),now())")
            .bind(id).bind(org).bind(code).bind(name).bind(description).bind(invoice_display_name)
            .execute(&self.pool).await?;
        Ok(self.find_product_category(id).await?.expect("just inserted"))
    }
    pub async fn update_product_category(&self, id: Uuid, name: Option<&str>, description: Option<&str>, invoice_display_name: Option<&str>) -> Result<Option<ProductCategoryRow>> {
        sqlx::query("UPDATE product_categories SET name = COALESCE($2, name), description = COALESCE($3, description), invoice_display_name = COALESCE($4, invoice_display_name), updated_at = now() WHERE id = $1")
            .bind(id).bind(name).bind(description).bind(invoice_display_name).execute(&self.pool).await?;
        self.find_product_category(id).await
    }
    pub async fn soft_delete_product_category(&self, id: Uuid) -> Result<()> {
        sqlx::query("UPDATE product_categories SET deleted_at = now(), updated_at = now() WHERE id = $1").bind(id).execute(&self.pool).await?;
        Ok(())
    }
    pub async fn find_product_category(&self, id: Uuid) -> Result<Option<ProductCategoryRow>> {
        let row = sqlx::query("SELECT id, code, name, description, invoice_display_name FROM product_categories WHERE id = $1 AND deleted_at IS NULL").bind(id).fetch_optional(&self.pool).await?;
        Ok(row.map(|r| (r.get("id"), r.get("code"), r.get("name"), r.get("description"), r.get("invoice_display_name"))))
    }
    pub async fn list_product_categories(&self, org: Uuid) -> Result<Vec<ProductCategoryRow>> {
        let rows = sqlx::query("SELECT id, code, name, description, invoice_display_name FROM product_categories WHERE organization_id = $1 AND deleted_at IS NULL ORDER BY created_at").bind(org).fetch_all(&self.pool).await?;
        Ok(rows.iter().map(|r| (r.get("id"), r.get("code"), r.get("name"), r.get("description"), r.get("invoice_display_name"))).collect())
    }

    // ---- catalog: products ----------------------------------------------

    #[allow(clippy::too_many_arguments)]
    pub async fn insert_product(&self, id: Uuid, org: Uuid, code: &str, name: &str, description: Option<&str>, invoice_display_name: Option<&str>, product_type: Option<&str>, billable_metric_id: Option<Uuid>, product_category_id: Option<Uuid>) -> Result<ProductRow> {
        sqlx::query("INSERT INTO products (id, organization_id, code, name, description, invoice_display_name, product_type, billable_metric_id, product_category_id, created_at, updated_at) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,now(),now())")
            .bind(id).bind(org).bind(code).bind(name).bind(description).bind(invoice_display_name).bind(product_type).bind(billable_metric_id).bind(product_category_id)
            .execute(&self.pool).await?;
        Ok(self.find_product(id).await?.expect("just inserted"))
    }
    pub async fn update_product(&self, id: Uuid, name: Option<&str>, description: Option<&str>, invoice_display_name: Option<&str>, product_category_id: Option<Uuid>) -> Result<Option<ProductRow>> {
        sqlx::query("UPDATE products SET name = COALESCE($2, name), description = COALESCE($3, description), invoice_display_name = COALESCE($4, invoice_display_name), product_category_id = COALESCE($5, product_category_id), updated_at = now() WHERE id = $1")
            .bind(id).bind(name).bind(description).bind(invoice_display_name).bind(product_category_id).execute(&self.pool).await?;
        self.find_product(id).await
    }
    pub async fn soft_delete_product(&self, id: Uuid) -> Result<()> {
        sqlx::query("UPDATE products SET deleted_at = now(), updated_at = now() WHERE id = $1").bind(id).execute(&self.pool).await?;
        Ok(())
    }
    pub async fn find_product(&self, id: Uuid) -> Result<Option<ProductRow>> {
        let row = sqlx::query("SELECT * FROM products WHERE id = $1 AND deleted_at IS NULL").bind(id).fetch_optional(&self.pool).await?;
        Ok(row.as_ref().map(product_row))
    }
    pub async fn list_products(&self, org: Uuid) -> Result<Vec<ProductRow>> {
        let rows = sqlx::query("SELECT * FROM products WHERE organization_id = $1 AND deleted_at IS NULL ORDER BY created_at").bind(org).fetch_all(&self.pool).await?;
        Ok(rows.iter().map(product_row).collect())
    }

    // ---- catalog: product filters ---------------------------------------

    #[allow(clippy::too_many_arguments)]
    pub async fn insert_product_filter(&self, id: Uuid, org: Uuid, product_id: Uuid, code: &str, name: &str, description: Option<&str>, invoice_display_name: Option<&str>, values: &Value) -> Result<ProductFilterRow> {
        sqlx::query("INSERT INTO product_filters (id, organization_id, product_id, code, name, description, invoice_display_name, values, created_at, updated_at) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,now(),now())")
            .bind(id).bind(org).bind(product_id).bind(code).bind(name).bind(description).bind(invoice_display_name).bind(values)
            .execute(&self.pool).await?;
        Ok(self.find_product_filter(id).await?.expect("just inserted"))
    }
    pub async fn update_product_filter(&self, id: Uuid, name: Option<&str>, invoice_display_name: Option<&str>, values: Option<&Value>) -> Result<Option<ProductFilterRow>> {
        sqlx::query("UPDATE product_filters SET name = COALESCE($2, name), invoice_display_name = COALESCE($3, invoice_display_name), values = COALESCE($4, values), updated_at = now() WHERE id = $1")
            .bind(id).bind(name).bind(invoice_display_name).bind(values).execute(&self.pool).await?;
        self.find_product_filter(id).await
    }
    pub async fn soft_delete_product_filter(&self, id: Uuid) -> Result<()> {
        sqlx::query("UPDATE product_filters SET deleted_at = now(), updated_at = now() WHERE id = $1").bind(id).execute(&self.pool).await?;
        Ok(())
    }
    pub async fn find_product_filter(&self, id: Uuid) -> Result<Option<ProductFilterRow>> {
        let row = sqlx::query("SELECT * FROM product_filters WHERE id = $1 AND deleted_at IS NULL").bind(id).fetch_optional(&self.pool).await?;
        Ok(row.as_ref().map(product_filter_row))
    }
    pub async fn list_product_filters(&self, product_id: Uuid) -> Result<Vec<ProductFilterRow>> {
        let rows = sqlx::query("SELECT * FROM product_filters WHERE product_id = $1 AND deleted_at IS NULL ORDER BY created_at").bind(product_id).fetch_all(&self.pool).await?;
        Ok(rows.iter().map(product_filter_row).collect())
    }

    // ---- catalog: rate cards + rates ------------------------------------

    #[allow(clippy::too_many_arguments)]
    pub async fn insert_rate_card(&self, id: Uuid, org: Uuid, product_id: Uuid, code: &str, name: &str, description: Option<&str>, currency: &str, billing_timing: Option<&str>, proration: bool, display_on_invoice: bool, wallet_targetable: bool) -> Result<RateCardRow> {
        sqlx::query("INSERT INTO rate_cards (id, organization_id, product_id, code, name, description, currency, billing_timing, proration, display_on_invoice, wallet_targetable, created_at, updated_at) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,now(),now())")
            .bind(id).bind(org).bind(product_id).bind(code).bind(name).bind(description).bind(currency).bind(billing_timing).bind(proration).bind(display_on_invoice).bind(wallet_targetable)
            .execute(&self.pool).await?;
        Ok(self.find_rate_card(id).await?.expect("just inserted"))
    }
    pub async fn update_rate_card(&self, id: Uuid, name: Option<&str>, description: Option<&str>, proration: Option<bool>, display_on_invoice: Option<bool>, wallet_targetable: Option<bool>) -> Result<Option<RateCardRow>> {
        sqlx::query("UPDATE rate_cards SET name = COALESCE($2, name), description = COALESCE($3, description), proration = COALESCE($4, proration), display_on_invoice = COALESCE($5, display_on_invoice), wallet_targetable = COALESCE($6, wallet_targetable), updated_at = now() WHERE id = $1")
            .bind(id).bind(name).bind(description).bind(proration).bind(display_on_invoice).bind(wallet_targetable).execute(&self.pool).await?;
        self.find_rate_card(id).await
    }
    pub async fn soft_delete_rate_card(&self, id: Uuid) -> Result<()> {
        sqlx::query("UPDATE rate_cards SET deleted_at = now(), updated_at = now() WHERE id = $1").bind(id).execute(&self.pool).await?;
        Ok(())
    }
    pub async fn find_rate_card(&self, id: Uuid) -> Result<Option<RateCardRow>> {
        let row = sqlx::query("SELECT * FROM rate_cards WHERE id = $1 AND deleted_at IS NULL").bind(id).fetch_optional(&self.pool).await?;
        Ok(row.as_ref().map(rate_card_row))
    }
    pub async fn list_rate_cards(&self, org: Uuid) -> Result<Vec<RateCardRow>> {
        let rows = sqlx::query("SELECT * FROM rate_cards WHERE organization_id = $1 AND deleted_at IS NULL ORDER BY created_at").bind(org).fetch_all(&self.pool).await?;
        Ok(rows.iter().map(rate_card_row).collect())
    }

    pub async fn insert_rate_card_rate(&self, id: Uuid, org: Uuid, rate_card_id: Uuid, charge_model: i32, properties: &Value) -> Result<RateCardRateRow> {
        sqlx::query("INSERT INTO rate_card_rates (id, organization_id, rate_card_id, charge_model, properties, created_at, updated_at) VALUES ($1,$2,$3,$4,$5,now(),now())")
            .bind(id).bind(org).bind(rate_card_id).bind(charge_model).bind(properties).execute(&self.pool).await?;
        Ok(self.find_rate_card_rate(id).await?.expect("just inserted"))
    }
    pub async fn update_rate_card_rate(&self, id: Uuid, charge_model: Option<i32>, properties: Option<&Value>) -> Result<Option<RateCardRateRow>> {
        sqlx::query("UPDATE rate_card_rates SET charge_model = COALESCE($2, charge_model), properties = COALESCE($3, properties), updated_at = now() WHERE id = $1")
            .bind(id).bind(charge_model).bind(properties).execute(&self.pool).await?;
        self.find_rate_card_rate(id).await
    }
    pub async fn soft_delete_rate_card_rate(&self, id: Uuid) -> Result<()> {
        sqlx::query("UPDATE rate_card_rates SET deleted_at = now(), updated_at = now() WHERE id = $1").bind(id).execute(&self.pool).await?;
        Ok(())
    }
    pub async fn find_rate_card_rate(&self, id: Uuid) -> Result<Option<RateCardRateRow>> {
        let row = sqlx::query("SELECT id, rate_card_id, charge_model, properties FROM rate_card_rates WHERE id = $1 AND deleted_at IS NULL").bind(id).fetch_optional(&self.pool).await?;
        Ok(row.map(|r| (r.get("id"), r.get("rate_card_id"), r.get("charge_model"), r.get("properties"))))
    }
    pub async fn list_rate_card_rates(&self, rate_card_id: Uuid) -> Result<Vec<RateCardRateRow>> {
        let rows = sqlx::query("SELECT id, rate_card_id, charge_model, properties FROM rate_card_rates WHERE rate_card_id = $1 AND deleted_at IS NULL ORDER BY created_at").bind(rate_card_id).fetch_all(&self.pool).await?;
        Ok(rows.iter().map(|r| (r.get("id"), r.get("rate_card_id"), r.get("charge_model"), r.get("properties"))).collect())
    }

    // ---- catalog: plan-applied rate cards + rate phases -----------------

    pub async fn insert_plan_applied_rate_card(&self, id: Uuid, org: Uuid, plan_id: Uuid, rate_card_id: Uuid) -> Result<PlanAppliedRateCardRow> {
        sqlx::query("INSERT INTO plan_applied_rate_cards (id, organization_id, plan_id, rate_card_id, created_at, updated_at) VALUES ($1,$2,$3,$4,now(),now())")
            .bind(id).bind(org).bind(plan_id).bind(rate_card_id).execute(&self.pool).await?;
        Ok((id, plan_id, rate_card_id))
    }
    pub async fn soft_delete_plan_applied_rate_card(&self, id: Uuid) -> Result<()> {
        sqlx::query("UPDATE plan_applied_rate_cards SET deleted_at = now(), updated_at = now() WHERE id = $1").bind(id).execute(&self.pool).await?;
        Ok(())
    }
    pub async fn list_plan_applied_rate_cards(&self, plan_id: Uuid) -> Result<Vec<PlanAppliedRateCardRow>> {
        let rows = sqlx::query("SELECT id, plan_id, rate_card_id FROM plan_applied_rate_cards WHERE plan_id = $1 AND deleted_at IS NULL ORDER BY created_at").bind(plan_id).fetch_all(&self.pool).await?;
        Ok(rows.iter().map(|r| (r.get("id"), r.get("plan_id"), r.get("rate_card_id"))).collect())
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn insert_rate_phase(&self, id: Uuid, org: Uuid, pa_rate_card_id: Uuid, code: &str, name: Option<&str>, position: Option<i32>, cycle_count: Option<i32>) -> Result<RatePhaseRow> {
        sqlx::query("INSERT INTO rate_phases (id, organization_id, plan_applied_rate_card_id, code, name, position, billing_interval_cycle_count, created_at, updated_at) VALUES ($1,$2,$3,$4,$5,$6,$7,now(),now())")
            .bind(id).bind(org).bind(pa_rate_card_id).bind(code).bind(name).bind(position).bind(cycle_count).execute(&self.pool).await?;
        Ok(self.find_rate_phase(id).await?.expect("just inserted"))
    }
    pub async fn update_rate_phase(&self, id: Uuid, name: Option<&str>, position: Option<i32>, cycle_count: Option<i32>) -> Result<Option<RatePhaseRow>> {
        sqlx::query("UPDATE rate_phases SET name = COALESCE($2, name), position = COALESCE($3, position), billing_interval_cycle_count = COALESCE($4, billing_interval_cycle_count), updated_at = now() WHERE id = $1")
            .bind(id).bind(name).bind(position).bind(cycle_count).execute(&self.pool).await?;
        self.find_rate_phase(id).await
    }
    pub async fn soft_delete_rate_phase(&self, id: Uuid) -> Result<()> {
        sqlx::query("UPDATE rate_phases SET deleted_at = now(), updated_at = now() WHERE id = $1").bind(id).execute(&self.pool).await?;
        Ok(())
    }
    pub async fn find_rate_phase(&self, id: Uuid) -> Result<Option<RatePhaseRow>> {
        let row = sqlx::query("SELECT * FROM rate_phases WHERE id = $1 AND deleted_at IS NULL").bind(id).fetch_optional(&self.pool).await?;
        Ok(row.as_ref().map(rate_phase_row))
    }
    pub async fn list_rate_phases(&self, pa_rate_card_id: Uuid) -> Result<Vec<RatePhaseRow>> {
        let rows = sqlx::query("SELECT * FROM rate_phases WHERE plan_applied_rate_card_id = $1 AND deleted_at IS NULL ORDER BY position NULLS LAST, created_at").bind(pa_rate_card_id).fetch_all(&self.pool).await?;
        Ok(rows.iter().map(rate_phase_row).collect())
    }

    // ---- catalog: pricing units + commitments ---------------------------

    pub async fn insert_pricing_unit(&self, id: Uuid, org: Uuid, code: &str, name: &str, short_name: &str, description: Option<&str>) -> Result<PricingUnitRow> {
        sqlx::query("INSERT INTO pricing_units (id, organization_id, code, name, short_name, description, created_at, updated_at) VALUES ($1,$2,$3,$4,$5,$6,now(),now())")
            .bind(id).bind(org).bind(code).bind(name).bind(short_name).bind(description).execute(&self.pool).await?;
        Ok(self.find_pricing_unit(id).await?.expect("just inserted"))
    }
    pub async fn update_pricing_unit(&self, id: Uuid, name: Option<&str>, short_name: Option<&str>, description: Option<&str>) -> Result<Option<PricingUnitRow>> {
        sqlx::query("UPDATE pricing_units SET name = COALESCE($2, name), short_name = COALESCE($3, short_name), description = COALESCE($4, description), updated_at = now() WHERE id = $1")
            .bind(id).bind(name).bind(short_name).bind(description).execute(&self.pool).await?;
        self.find_pricing_unit(id).await
    }
    pub async fn find_pricing_unit(&self, id: Uuid) -> Result<Option<PricingUnitRow>> {
        let row = sqlx::query("SELECT id, code, name, short_name, description FROM pricing_units WHERE id = $1 AND deleted_at IS NULL").bind(id).fetch_optional(&self.pool).await?;
        Ok(row.map(|r| (r.get("id"), r.get("code"), r.get("name"), r.get("short_name"), r.get("description"))))
    }
    pub async fn list_pricing_units(&self, org: Uuid) -> Result<Vec<PricingUnitRow>> {
        let rows = sqlx::query("SELECT id, code, name, short_name, description FROM pricing_units WHERE organization_id = $1 AND deleted_at IS NULL ORDER BY created_at").bind(org).fetch_all(&self.pool).await?;
        Ok(rows.iter().map(|r| (r.get("id"), r.get("code"), r.get("name"), r.get("short_name"), r.get("description"))).collect())
    }

    pub async fn insert_commitment(&self, id: Uuid, org: Uuid, plan_id: Option<Uuid>, commitment_type: &str, amount_cents: i64, invoice_display_name: Option<&str>) -> Result<CommitmentRow> {
        sqlx::query("INSERT INTO commitments (id, organization_id, plan_id, commitment_type, amount_cents, invoice_display_name, created_at, updated_at) VALUES ($1,$2,$3,$4,$5,$6,now(),now())")
            .bind(id).bind(org).bind(plan_id).bind(commitment_type).bind(amount_cents).bind(invoice_display_name).execute(&self.pool).await?;
        Ok(self.find_commitment(id).await?.expect("just inserted"))
    }
    pub async fn update_commitment(&self, id: Uuid, amount_cents: Option<i64>, invoice_display_name: Option<&str>) -> Result<Option<CommitmentRow>> {
        sqlx::query("UPDATE commitments SET amount_cents = COALESCE($2, amount_cents), invoice_display_name = COALESCE($3, invoice_display_name), updated_at = now() WHERE id = $1")
            .bind(id).bind(amount_cents).bind(invoice_display_name).execute(&self.pool).await?;
        self.find_commitment(id).await
    }
    pub async fn soft_delete_commitment(&self, id: Uuid) -> Result<()> {
        sqlx::query("UPDATE commitments SET deleted_at = now(), updated_at = now() WHERE id = $1").bind(id).execute(&self.pool).await?;
        Ok(())
    }
    pub async fn find_commitment(&self, id: Uuid) -> Result<Option<CommitmentRow>> {
        let row = sqlx::query("SELECT id, plan_id, commitment_type, amount_cents, invoice_display_name FROM commitments WHERE id = $1 AND deleted_at IS NULL").bind(id).fetch_optional(&self.pool).await?;
        Ok(row.map(|r| (r.get("id"), r.get("plan_id"), r.get("commitment_type"), r.get("amount_cents"), r.get("invoice_display_name"))))
    }
    pub async fn list_commitments(&self, plan_id: Uuid) -> Result<Vec<CommitmentRow>> {
        let rows = sqlx::query("SELECT id, plan_id, commitment_type, amount_cents, invoice_display_name FROM commitments WHERE plan_id = $1 AND deleted_at IS NULL ORDER BY created_at").bind(plan_id).fetch_all(&self.pool).await?;
        Ok(rows.iter().map(|r| (r.get("id"), r.get("plan_id"), r.get("commitment_type"), r.get("amount_cents"), r.get("invoice_display_name"))).collect())
    }
}
