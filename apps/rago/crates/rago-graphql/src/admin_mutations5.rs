//! GraphQL admin mutations — slice 5 (final tail): quotes/orders, AI + finance
//! assistant, data exports, SSO login shapes, EntraId integration, and
//! tax-provider retries.
//!
//! The AI/finance resolvers answer through the configured OpenAI-compatible
//! endpoint (`rago_jobs::assistant`); without `RAGO_ASSISTANT_API_KEY` they
//! persist the question with status `not_configured` and never fabricate a
//! reply. SSO login exchanges the code with the configured IdP
//! (`rago_jobs::sso`) and signs in an existing user; an unconfigured IdP is a
//! structured `idp_not_configured` error. Sync/export/tax-retry mutations
//! enqueue the durable jobs the worker runs.

use async_graphql::{Context, Object, Result, ID};
use chrono::Utc;
use serde_json::json;
use uuid::Uuid;

use rago_integrations::webhooks::WebhookKind;

use crate::admin_inputs5::*;
use crate::context::{db, require_org};
use crate::types::{CreditNote, Integration, Invoice};
use crate::types_admin5::*;
use crate::webhook_emit::emit;

fn uuid(id: &ID) -> Result<Uuid> {
    Uuid::parse_str(id.as_str()).map_err(|_| async_graphql::Error::new("invalid_id"))
}
fn parse_ts(s: &str) -> Result<chrono::NaiveDateTime> {
    chrono::DateTime::parse_from_rfc3339(s).map(|d| d.naive_utc()).map_err(|_| async_graphql::Error::new("invalid_timestamp"))
}
fn number(prefix: &str) -> String {
    format!("{prefix}-{}", &Uuid::new_v4().to_string()[..8].to_uppercase())
}
/// Seal an SSO client secret for storage (`encryption_key_missing` when the
/// server has no `RAGO_ENCRYPTION_KEY`).
fn seal_secret(secret: Option<&str>) -> Result<Option<String>> {
    let Some(v) = secret.map(str::trim).filter(|v| !v.is_empty()) else {
        return Ok(None);
    };
    rago_db::secrets::seal(v).map(Some).map_err(|e| {
        async_graphql::Error::new(match e {
            rago_db::secrets::SecretError::KeyMissing => {
                "encryption_key_missing: set RAGO_ENCRYPTION_KEY to store integration credentials".to_string()
            }
            other => other.to_string(),
        })
    })
}

/// Normalize an SSO email domain (`@acme.com`, `ACME.com` → `acme.com`).
fn sso_domain(domain: &str) -> Result<String> {
    let d = rago_jobs::sso::email_domain(domain.trim_start_matches('@'));
    if d.is_empty() || !d.contains('.') {
        return Err(async_graphql::Error::new("invalid_domain"));
    }
    Ok(d)
}

/// Canonical IdP name.
fn normalize_idp(provider: &str) -> Result<&'static str> {
    match provider.trim().to_ascii_lowercase().as_str() {
        "google" => Ok("google"),
        "okta" => Ok("okta"),
        "entra_id" | "entraid" | "entra" | "azure" => Ok("entra_id"),
        other => Err(async_graphql::Error::new(format!("unsupported_idp: {other}"))),
    }
}

/// Admin CRUD mutation root — slice 5 (merged into the schema's `Mutation`).
#[derive(Default)]
pub struct AdminMutation5;

#[Object]
impl AdminMutation5 {
    // ---- quotes ----------------------------------------------------------

    /// `CreateQuote` — a draft quote plus its initial (v1) version.
    async fn create_quote(&self, ctx: &Context<'_>, input: CreateQuoteInput) -> Result<Quote> {
        let org = require_org(ctx)?;
        let customer = db(ctx)
            .find_customer(uuid(&input.customer_id)?)
            .await?
            .filter(|c| c.organization_id == org.id)
            .ok_or_else(|| async_graphql::Error::new("customer_not_found"))?;
        let line_items = input.line_items.map(|j| j.0).unwrap_or_else(|| json!([]));
        let total = input.total_amount_cents.unwrap_or(0);
        let id = Uuid::new_v4();
        db(ctx).insert_quote(id, org.id, customer.id, &number("Q"), &input.currency, total, 0, total, &line_items).await?;
        db(ctx).insert_quote_version(Uuid::new_v4(), org.id, id, 1, &input.currency, total, &line_items).await?;
        // `Quotes::CreateService` emits `quote.created`.
        emit(db(ctx), org.id, WebhookKind::QuoteCreated, json!({ "lago_id": id, "lago_customer_id": customer.id, "currency": input.currency, "total_amount_cents": total })).await;
        Ok(db(ctx).find_quote_full(id).await?.expect("just inserted").into())
    }

    /// `UpdateQuote`.
    async fn update_quote(&self, ctx: &Context<'_>, input: UpdateQuoteInput) -> Result<Quote> {
        require_org(ctx)?;
        let line_items = input.line_items.map(|j| j.0);
        let row = db(ctx).update_quote(uuid(&input.id)?, input.status.as_deref(), line_items.as_ref(), input.total_amount_cents).await?.ok_or_else(|| async_graphql::Error::new("quote_not_found"))?;
        Ok(row.into())
    }

    // ---- quote versions --------------------------------------------------

    /// `ApproveQuoteVersion`.
    async fn approve_quote_version(&self, ctx: &Context<'_>, id: ID, expires_at: Option<String>) -> Result<QuoteVersion> {
        let org = require_org(ctx)?;
        let expires = match expires_at.as_deref() {
            Some(s) => Some(parse_ts(s)?),
            None => None,
        };
        let vid = uuid(&id)?;
        let row = db(ctx).approve_quote_version(vid, expires).await?.ok_or_else(|| async_graphql::Error::new("quote_version_not_found"))?;
        // `Quotes::Versions::ApproveService` emits `quote.approved`.
        emit(db(ctx), org.id, WebhookKind::QuoteApproved, json!({ "lago_id": vid })).await;
        Ok(row.into())
    }

    /// `VoidQuoteVersion`.
    async fn void_quote_version(&self, ctx: &Context<'_>, id: ID) -> Result<QuoteVersion> {
        let org = require_org(ctx)?;
        let vid = uuid(&id)?;
        let row = db(ctx).void_quote_version(vid).await?.ok_or_else(|| async_graphql::Error::new("quote_version_not_found"))?;
        // `Quotes::Versions::VoidService` emits `quote.voided`.
        emit(db(ctx), org.id, WebhookKind::QuoteVoided, json!({ "lago_id": vid })).await;
        Ok(row.into())
    }

    /// `CloneQuoteVersion`.
    async fn clone_quote_version(&self, ctx: &Context<'_>, id: ID) -> Result<QuoteVersion> {
        require_org(ctx)?;
        let row = db(ctx).clone_quote_version(uuid(&id)?).await?.ok_or_else(|| async_graphql::Error::new("quote_version_not_found"))?;
        Ok(row.into())
    }

    /// `UpdateQuoteVersion`.
    async fn update_quote_version(&self, ctx: &Context<'_>, id: ID, line_items: async_graphql::Json<serde_json::Value>, total_amount_cents: i64) -> Result<QuoteVersion> {
        require_org(ctx)?;
        let row = db(ctx).update_quote_version(uuid(&id)?, &line_items.0, total_amount_cents).await?.ok_or_else(|| async_graphql::Error::new("quote_version_not_found_or_not_draft"))?;
        Ok(row.into())
    }

    // ---- order forms + orders -------------------------------------------

    /// `MarkOrderFormAsSigned`.
    async fn mark_order_form_as_signed(&self, ctx: &Context<'_>, id: ID, signed_document: Option<String>, execution_mode: Option<String>, execute_at: Option<String>) -> Result<OrderForm> {
        let org = require_org(ctx)?;
        let at = match execute_at.as_deref() {
            Some(s) => Some(parse_ts(s)?),
            None => None,
        };
        let ofid = uuid(&id)?;
        let current = db(ctx).find_order_form(ofid).await?.ok_or_else(|| async_graphql::Error::new("order_form_not_found"))?;
        if db(ctx).order_form_origin(ofid).await?.map(|(o, _)| o) != Some(org.id) {
            return Err(async_graphql::Error::new("order_form_not_found"));
        }
        if current.3 == "voided" {
            return Err(async_graphql::Error::new("order_form_voided"));
        }
        let row = db(ctx).mark_order_form_signed(ofid, signed_document.as_deref(), execution_mode.as_deref(), at).await?.ok_or_else(|| async_graphql::Error::new("order_form_not_found"))?;
        // `OrderForms::MarkAsSignedService` emits `order_form.signed` and
        // creates the order (`order.created`).
        emit(db(ctx), org.id, WebhookKind::OrderFormSigned, json!({ "lago_id": ofid })).await;
        rago_jobs::orders::create_order_for_signed_form(db(ctx), org.id, ofid)
            .await
            .map_err(crate::admin_mutations6::order_flow_error)?;
        Ok(row.into())
    }

    /// `VoidOrderForm`.
    async fn void_order_form(&self, ctx: &Context<'_>, id: ID) -> Result<OrderForm> {
        let org = require_org(ctx)?;
        let ofid = uuid(&id)?;
        let row = db(ctx).void_order_form(ofid).await?.ok_or_else(|| async_graphql::Error::new("order_form_not_found"))?;
        // `OrderForms::VoidService` emits `order_form.voided`.
        emit(db(ctx), org.id, WebhookKind::OrderFormVoided, json!({ "lago_id": ofid })).await;
        Ok(row.into())
    }

    /// `ExecuteOrder`.
    async fn execute_order(&self, ctx: &Context<'_>, id: ID) -> Result<Order> {
        let org = require_org(ctx)?;
        let oid = uuid(&id)?;
        // `Orders::ExecuteService`: invoice the quote's line items, accept the
        // quote, queue collection, and emit `order.executed`.
        let (row, _invoice) = rago_jobs::orders::execute_order(db(ctx), org.id, oid)
            .await
            .map_err(crate::admin_mutations6::order_flow_error)?;
        Ok(row.into())
    }

    /// `UpdateOrder`.
    async fn update_order(&self, ctx: &Context<'_>, id: ID, status: String) -> Result<Order> {
        require_org(ctx)?;
        let row = db(ctx).update_order_status(uuid(&id)?, &status).await?.ok_or_else(|| async_graphql::Error::new("order_not_found"))?;
        Ok(row.into())
    }

    // ---- AI + finance assistant -----------------------------------------

    /// `CreateAiConversation` — append the user message to a conversation and,
    /// when the assistant is configured (`RAGO_ASSISTANT_API_KEY`), the
    /// assistant's reply grounded in the org's billing figures. Without a key
    /// no reply is fabricated; the conversation holds the user message only.
    async fn create_ai_conversation(&self, ctx: &Context<'_>, message: String, conversation_id: Option<ID>) -> Result<AiConversation> {
        let org = require_org(ctx)?;
        let conv_id = match &conversation_id {
            Some(c) => uuid(c)?,
            None => {
                let id = Uuid::new_v4();
                db(ctx).insert_ai_conversation(id, org.id, None, Some(&message)).await?;
                id
            }
        };
        db(ctx).insert_ai_message(Uuid::new_v4(), org.id, conv_id, "user", &message).await?;
        if rago_jobs::assistant::is_configured() {
            let history: Vec<(String, String)> = db(ctx)
                .list_ai_messages(conv_id)
                .await?
                .into_iter()
                .map(|(_, role, content)| (role, content))
                .collect();
            let transport = rago_integrations::default_transport();
            let reply = rago_jobs::assistant::answer(db(ctx), &transport, org.id, &history)
                .await
                .map_err(|e| async_graphql::Error::new(format!("assistant_error: {e}")))?;
            db(ctx).insert_ai_message(Uuid::new_v4(), org.id, conv_id, "assistant", &reply).await?;
        }
        let conv = db(ctx).find_ai_conversation(conv_id).await?.ok_or_else(|| async_graphql::Error::new("conversation_not_found"))?;
        let messages = db(ctx).list_ai_messages(conv_id).await?.into_iter().map(Into::into).collect();
        Ok(AiConversation { id: conv.id.into(), name: conv.name, messages })
    }

    /// `AskFinanceAssistant` — answer the question with the configured
    /// assistant (status `answered`), record a provider failure as `failed`, or
    /// `not_configured` without a key — never a fabricated answer.
    async fn ask_finance_assistant(&self, ctx: &Context<'_>, question: String, session_id: Option<ID>) -> Result<FinanceAnswer> {
        let org = require_org(ctx)?;
        let session = match &session_id {
            Some(s) => uuid(s)?,
            None => Uuid::new_v4(),
        };
        let (answer, status) = if rago_jobs::assistant::is_configured() {
            let transport = rago_integrations::default_transport();
            let history = vec![("user".to_string(), question.clone())];
            match rago_jobs::assistant::answer(db(ctx), &transport, org.id, &history).await {
                Ok(a) => (Some(a), "answered"),
                Err(e) => (Some(format!("The assistant could not answer: {e}")), "failed"),
            }
        } else {
            (None, "not_configured")
        };
        db(ctx).insert_finance_assistant_message(Uuid::new_v4(), org.id, session, &question, answer.as_deref(), status).await?;
        Ok(FinanceAnswer { session_id: session.into(), question, answer, status: status.to_string() })
    }

    /// `ExportFinanceAssistantResult` — enqueue an export of a session's answers.
    async fn export_finance_assistant_result(&self, ctx: &Context<'_>, session_id: ID) -> Result<DataExport> {
        let org = require_org(ctx)?;
        let id = Uuid::new_v4();
        db(ctx).insert_data_export_filtered(id, org.id, "finance_assistant", "csv", &json!({ "session_id": session_id.as_str() })).await?;
        db(ctx).enqueue_job(rago_jobs::JOB_DATA_EXPORT_GENERATE, &json!({ "data_export_id": id }), Utc::now().naive_utc(), 5).await?;
        Ok(DataExport { id: id.into(), resource_type: "finance_assistant".into(), format: "csv".into(), status: "pending".into(), row_count: 0 })
    }

    // ---- data exports ----------------------------------------------------

    /// `CreateInvoicesDataExport`.
    async fn create_invoices_data_export(&self, ctx: &Context<'_>, format: Option<String>, filters: Option<async_graphql::Json<serde_json::Value>>) -> Result<DataExport> {
        self.data_export(ctx, "invoices", format, filters).await
    }

    /// `CreateCreditNotesDataExport`.
    async fn create_credit_notes_data_export(&self, ctx: &Context<'_>, format: Option<String>, filters: Option<async_graphql::Json<serde_json::Value>>) -> Result<DataExport> {
        self.data_export(ctx, "credit_notes", format, filters).await
    }

    // ---- SSO login --------------------------------------------------------

    /// `SsoAuthorize` — start an SSO login. The organization is resolved from
    /// the sign-in `email` (its domain must be served by an org's `auth`
    /// integration); `provider` (`google`, `okta`, `entra_id`) narrows the IdP.
    /// Mints a single-use `state` bound to that org + a PKCE verifier and
    /// returns the IdP authorize URL. `sso_not_enabled_for_domain` when no
    /// organization enabled SSO for the domain; `idp_not_configured: ...` when
    /// the IdP app credentials are missing.
    async fn sso_authorize(&self, ctx: &Context<'_>, provider: Option<String>, email: Option<String>) -> Result<crate::types_admin4::SsoAuthorization> {
        let provider = provider.as_deref().map(normalize_idp).transpose()?;
        let started = rago_jobs::sso::start(db(ctx), provider, email.as_deref())
            .await
            .map_err(|e| async_graphql::Error::new(e.to_string()))?;
        Ok(crate::types_admin4::SsoAuthorization { url: started.authorize_url, state: started.state, provider: started.provider })
    }

    /// `OktaLogin` — verifies the server-issued `state`, then exchanges the
    /// auth code (with its PKCE verifier) with the organization's Okta tenant.
    async fn okta_login(&self, ctx: &Context<'_>, code: String, state: String) -> Result<crate::types_admin4::LoginPayload> {
        Self::sso(ctx, Some("okta"), &code, &state).await
    }

    /// `GoogleLoginUser` — `state` is required (issued by `ssoAuthorize`).
    async fn google_login(&self, ctx: &Context<'_>, code: String, state: Option<String>) -> Result<crate::types_admin4::LoginPayload> {
        let state = state.ok_or_else(|| async_graphql::Error::new("sso_state_missing"))?;
        Self::sso(ctx, Some("google"), &code, &state).await
    }

    /// `EntraIdLogin`.
    async fn entra_id_login(&self, ctx: &Context<'_>, code: String, state: String) -> Result<crate::types_admin4::LoginPayload> {
        Self::sso(ctx, Some("entra_id"), &code, &state).await
    }

    /// `SsoLogin` — provider-agnostic callback: the provider is the one the
    /// `state` was minted for.
    async fn sso_login(&self, ctx: &Context<'_>, code: String, state: String) -> Result<crate::types_admin4::LoginPayload> {
        Self::sso(ctx, None, &code, &state).await
    }

    /// `CreateGoogleIntegration` — enable "Sign in with Google" for the
    /// organization's email `domain`. Client credentials are optional: without
    /// them the deployment's Google app (`RAGO_GOOGLE_CLIENT_ID`) is used.
    async fn create_google_integration(&self, ctx: &Context<'_>, name: String, domain: String, code: Option<String>, client_id: Option<String>, client_secret: Option<String>, enforce_sso: Option<bool>) -> Result<Integration> {
        let org = require_org(ctx)?;
        let domain = sso_domain(&domain)?;
        let settings = json!({
            "domain": domain,
            "client_id": client_id,
            "client_secret_ref": client_secret.as_deref().map(rago_db::secrets::mask),
            "client_secret_sealed": seal_secret(client_secret.as_deref())?,
            "enforce_sso": enforce_sso.unwrap_or(false),
        });
        let code = code.unwrap_or_else(|| format!("google_{domain}"));
        let row = db(ctx).insert_integration(Uuid::new_v4(), org.id, "auth", "google", Some(&code), Some(&name), &settings).await?;
        Ok(row.into())
    }

    /// `CreateEntraIdIntegration` — enable Microsoft Entra ID (Azure AD) SSO
    /// for the organization's email `domain`. The client secret is sealed at
    /// rest (only its last four characters are shown afterwards).
    async fn create_entra_id_integration(&self, ctx: &Context<'_>, code: String, name: String, tenant_id: Option<String>, client_id: Option<String>, client_secret: Option<String>, domain: Option<String>, enforce_sso: Option<bool>) -> Result<Integration> {
        let org = require_org(ctx)?;
        let domain = domain.as_deref().map(sso_domain).transpose()?;
        let settings = json!({
            "domain": domain,
            "tenant_id": tenant_id,
            "client_id": client_id,
            "client_secret_ref": client_secret.as_deref().map(rago_db::secrets::mask),
            "client_secret_sealed": seal_secret(client_secret.as_deref())?,
            "enforce_sso": enforce_sso.unwrap_or(false),
        });
        let row = db(ctx).insert_integration(Uuid::new_v4(), org.id, "auth", "entra_id", Some(&code), Some(&name), &settings).await?;
        Ok(row.into())
    }

    // ---- tax-provider retries -------------------------------------------

    /// `RetryTaxProviderVoiding` — enqueue the provider-tax void-sync retry.
    async fn retry_tax_provider_voiding(&self, ctx: &Context<'_>, id: ID) -> Result<Invoice> {
        let org = require_org(ctx)?;
        let iid = uuid(&id)?;
        let invoice = db(ctx).find_invoice(iid).await?.filter(|i| i.organization_id == org.id).ok_or_else(|| async_graphql::Error::new("invoice_not_found"))?;
        db(ctx).enqueue_job("invoices.tax_provider_voiding", &json!({ "invoice_id": iid }), Utc::now().naive_utc(), 5).await?;
        let fees = db(ctx).fees_for_invoice(invoice.id).await?;
        Ok(Invoice { invoice, fees })
    }

    /// `RetryTaxReporting` — enqueue the credit-note tax-reporting retry.
    async fn retry_tax_reporting(&self, ctx: &Context<'_>, id: ID) -> Result<CreditNote> {
        let org = require_org(ctx)?;
        let cid = uuid(&id)?;
        let cn = db(ctx).find_credit_note(cid).await?.filter(|c| c.organization_id == org.id).ok_or_else(|| async_graphql::Error::new("credit_note_not_found"))?;
        db(ctx).enqueue_job("credit_notes.tax_reporting", &json!({ "credit_note_id": cid }), Utc::now().naive_utc(), 5).await?;
        Ok(CreditNote(cn))
    }

    // ---- integration update specialization ------------------------------

    /// `Update*Integration` (settings merge) — updates an integration's name and
    /// merges provider settings by id (the shared implementation behind the
    /// per-provider update graphql names).
    async fn update_integration_settings(&self, ctx: &Context<'_>, id: ID, name: Option<String>, settings: async_graphql::Json<serde_json::Value>) -> Result<Integration> {
        require_org(ctx)?;
        let row = db(ctx).update_integration(uuid(&id)?, None, name.as_deref(), Some(&settings.0)).await?.ok_or_else(|| async_graphql::Error::new("integration_not_found"))?;
        Ok(row.into())
    }
}

impl AdminMutation5 {
    /// OAuth code exchange with the IdP → verified email → existing member of
    /// the organization that enabled this IdP for the email's domain → session
    /// JWT pinned to that organization. SSO never creates users.
    async fn sso(ctx: &Context<'_>, provider: Option<&str>, code: &str, state: &str) -> Result<crate::types_admin4::LoginPayload> {
        let transport = rago_integrations::default_transport();
        let identity = rago_jobs::sso::complete(db(ctx), &transport, provider, code, state)
            .await
            .map_err(|e| async_graphql::Error::new(e.to_string()))?;
        let login = rago_jobs::sso::resolve_login(db(ctx), &identity)
            .await
            .map_err(|e| async_graphql::Error::new(e.to_string()))?;
        Ok(crate::types_admin4::LoginPayload {
            token: crate::auth::issue_sso_jwt(login.user_id, login.organization_id, &identity.provider),
            user: crate::types_admin4::User { id: login.user_id.into(), email: login.email },
        })
    }

    async fn data_export(&self, ctx: &Context<'_>, resource_type: &str, format: Option<String>, filters: Option<async_graphql::Json<serde_json::Value>>) -> Result<DataExport> {
        let org = require_org(ctx)?;
        let id = Uuid::new_v4();
        let fmt = format.unwrap_or_else(|| "csv".into());
        let filters = filters.map(|j| j.0).unwrap_or_else(|| json!({}));
        db(ctx).insert_data_export_filtered(id, org.id, resource_type, &fmt, &filters).await?;
        db(ctx).enqueue_job("data_exports.generate", &json!({ "data_export_id": id }), Utc::now().naive_utc(), 5).await?;
        Ok(DataExport { id: id.into(), resource_type: resource_type.into(), format: fmt, status: "pending".into(), row_count: 0 })
    }
}
