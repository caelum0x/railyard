use aitp_client::{AmountMicros, PaymentChallenge, Scheme};
use relay_contract::{AitpChallenge, AitpScheme, Microdollars};

use crate::{AitpError, Result};

pub fn to_client_challenge(challenge: &AitpChallenge) -> Result<PaymentChallenge> {
    Ok(PaymentChallenge {
        payee: challenge.payee.clone(),
        amount_micros: AmountMicros::new(challenge.amount_micros.get()),
        currency: challenge.currency.clone(),
        resource: challenge.resource.clone(),
        nonce: challenge.nonce.clone(),
        expiry: i64::try_from(challenge.expiry)
            .map_err(|_| AitpError::Wire("challenge expiry exceeds i64".into()))?,
        scheme: match challenge.scheme {
            AitpScheme::Hold => Scheme::Hold,
        },
    })
}

pub fn from_client_challenge(challenge: &PaymentChallenge) -> Result<AitpChallenge> {
    let scheme = match challenge.scheme {
        Scheme::Hold => AitpScheme::Hold,
        Scheme::Transfer => {
            return Err(AitpError::Wire(
                "Relay only supports metered aitp-hold challenges".into(),
            ));
        }
    };
    Ok(AitpChallenge {
        payee: challenge.payee.clone(),
        amount_micros: Microdollars::new(challenge.amount_micros.get()),
        currency: challenge.currency.clone(),
        resource: challenge.resource.clone(),
        nonce: challenge.nonce.clone(),
        expiry: u64::try_from(challenge.expiry)
            .map_err(|_| AitpError::Wire("challenge expiry is negative".into()))?,
        scheme,
    })
}
