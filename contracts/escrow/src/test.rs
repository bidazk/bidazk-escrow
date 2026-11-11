#![cfg(test)]
extern crate std;

use super::*;
use soroban_sdk::{
    testutils::{Address as _, AuthorizedFunction, AuthorizedInvocation, Ledger, MockAuth, MockAuthInvoke},
    token::{StellarAssetClient, TokenClient},
    Address, Env, IntoVal, Symbol,
};

const AMOUNT: i128 = 1_000;
const DELIVERY: u64 = 3_600;
const INSPECTION: u64 = 600;
const START: u64 = 1_000_000;

struct Setup<'a> {
    env: Env,
    escrow: EscrowClient<'a>,
    escrow_id: Address,
    token: TokenClient<'a>,
    buyer: Address,
    seller: Address,
    arbiter: Address,
}

fn setup<'a>() -> Setup<'a> {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(START);

    let issuer = Address::generate(&env);
    let sac = env.register_stellar_asset_contract_v2(issuer);
    let token_id = sac.address();

    let buyer = Address::generate(&env);
    let seller = Address::generate(&env);
    let arbiter = Address::generate(&env);
    StellarAssetClient::new(&env, &token_id).mint(&buyer, &(AMOUNT * 10));

    let escrow_id = env.register(Escrow, (arbiter.clone(), token_id.clone()));
    let escrow = EscrowClient::new(&env, &escrow_id);
    let token = TokenClient::new(&env, &token_id);

    Setup { env, escrow, escrow_id, token, buyer, seller, arbiter }
}

impl Setup<'_> {
    fn create(&self) -> u64 {
        self.escrow
            .create(&self.buyer, &self.seller, &AMOUNT, &DELIVERY, &INSPECTION)
    }

    fn advance(&self, secs: u64) {
        let now = self.env.ledger().timestamp();
        self.env.ledger().set_timestamp(now + secs);
    }

    fn status(&self, id: u64) -> Status {
        self.escrow.get_deal(&id).status
    }

    fn balance(&self, who: &Address) -> i128 {
        self.token.balance(who)
    }

    /// Asserts the only authorization the last call required was `who` signing `function`.
    fn assert_signed_by(&self, who: &Address, function: &str, args: soroban_sdk::Vec<soroban_sdk::Val>) {
        assert_eq!(
            self.env.auths(),
            std::vec![(
                who.clone(),
                AuthorizedInvocation {
                    function: AuthorizedFunction::Contract((
                        self.escrow_id.clone(),
                        Symbol::new(&self.env, function),
                        args,
                    )),
                    sub_invocations: std::vec![],
                }
            )]
        );
    }
}

// ---------------------------------------------------------------- happy paths

#[test]
fn create_locks_funds_in_contract() {
    let s = setup();
    let id = s.create();

    assert_eq!(id, 0);
    assert_eq!(s.status(id), Status::Funded);
    assert_eq!(s.balance(&s.escrow_id), AMOUNT);
    assert_eq!(s.balance(&s.buyer), AMOUNT * 9);

    let deal = s.escrow.get_deal(&id);
    assert_eq!(deal.delivery_deadline, START + DELIVERY);
    assert_eq!(deal.inspection_period, INSPECTION);
}

#[test]
fn deal_ids_increment() {
    let s = setup();
    assert_eq!(s.create(), 0);
    assert_eq!(s.create(), 1);
    assert_eq!(s.escrow.deal_count(), 2);
}

#[test]
fn buyer_confirms_after_delivery_pays_seller() {
    let s = setup();
    let id = s.create();
    s.escrow.mark_delivered(&id);
    s.escrow.confirm(&id);

    assert_eq!(s.status(id), Status::Released);
    assert_eq!(s.balance(&s.seller), AMOUNT);
    assert_eq!(s.balance(&s.escrow_id), 0);
}

#[test]
fn buyer_can_confirm_before_seller_marks_delivery() {
    // In-person pickup: the buyer has the item before the seller touches the app.
    let s = setup();
    let id = s.create();
    s.escrow.confirm(&id);

    assert_eq!(s.status(id), Status::Released);
    assert_eq!(s.balance(&s.seller), AMOUNT);
}

// ---------------------------------------------------------------- silent seller

#[test]
fn silent_seller_buyer_refunded_after_deadline() {
    let s = setup();
    let id = s.create();
    s.advance(DELIVERY + 1);
    s.escrow.claim_refund(&id);

    assert_eq!(s.status(id), Status::Refunded);
    assert_eq!(s.balance(&s.buyer), AMOUNT * 10);
    assert_eq!(s.balance(&s.escrow_id), 0);
    assert_eq!(s.escrow.get_deal(&id).buyer_refund, AMOUNT);
}

#[test]
fn refund_blocked_before_deadline() {
    // The rider may be on the way: the buyer cannot pull the money mid-delivery.
    let s = setup();
    let id = s.create();
    s.advance(DELIVERY);

    assert_eq!(s.escrow.try_claim_refund(&id), Err(Ok(Error::DeliveryDeadlineNotPassed)));
}

#[test]
fn refund_blocked_once_seller_marked_delivered() {
    // Buyer received the item, then claims "never delivered" after the deadline.
    let s = setup();
    let id = s.create();
    s.escrow.mark_delivered(&id);
    s.advance(DELIVERY + 1);

    assert_eq!(s.escrow.try_claim_refund(&id), Err(Ok(Error::InvalidState)));
}

#[test]
fn seller_cannot_mark_delivered_after_deadline() {
    let s = setup();
    let id = s.create();
    s.advance(DELIVERY + 1);

    assert_eq!(s.escrow.try_mark_delivered(&id), Err(Ok(Error::DeliveryDeadlinePassed)));
}

#[test]
fn seller_can_mark_delivered_exactly_at_deadline() {
    let s = setup();
    let id = s.create();
    s.advance(DELIVERY);
    s.escrow.mark_delivered(&id);

    assert_eq!(s.status(id), Status::Delivered);
}

// ---------------------------------------------------------------- silent buyer

#[test]
fn silent_buyer_seller_paid_after_inspection() {
    let s = setup();
    let id = s.create();
    s.escrow.mark_delivered(&id);
    s.advance(INSPECTION);
    s.escrow.claim_release(&id);

    assert_eq!(s.status(id), Status::Released);
    assert_eq!(s.balance(&s.seller), AMOUNT);
}

#[test]
fn release_blocked_during_inspection() {
    let s = setup();
    let id = s.create();
    s.escrow.mark_delivered(&id);
    s.advance(INSPECTION - 1);

    assert_eq!(s.escrow.try_claim_release(&id), Err(Ok(Error::InspectionNotOver)));
}

#[test]
fn release_blocked_before_delivery_marked() {
    let s = setup();
    let id = s.create();
    s.advance(DELIVERY * 10);

    assert_eq!(s.escrow.try_claim_release(&id), Err(Ok(Error::InvalidState)));
}

// ---------------------------------------------------------------- seller cancels

#[test]
fn seller_cancel_refunds_buyer_immediately() {
    let s = setup();
    let id = s.create();
    s.escrow.cancel(&id);

    assert_eq!(s.status(id), Status::Cancelled);
    assert_eq!(s.balance(&s.buyer), AMOUNT * 10);
}

#[test]
fn seller_cannot_cancel_after_marking_delivered() {
    let s = setup();
    let id = s.create();
    s.escrow.mark_delivered(&id);

    assert_eq!(s.escrow.try_cancel(&id), Err(Ok(Error::InvalidState)));
}

// ---------------------------------------------------------------- disputes

#[test]
fn dispute_freezes_money() {
    let s = setup();
    let id = s.create();
    s.escrow.mark_delivered(&id);
    s.escrow.dispute(&id);
    s.advance(INSPECTION * 10);

    assert_eq!(s.status(id), Status::Disputed);
    assert_eq!(s.escrow.try_claim_release(&id), Err(Ok(Error::InvalidState)));
    assert_eq!(s.escrow.try_confirm(&id), Err(Ok(Error::InvalidState)));
    assert_eq!(s.balance(&s.escrow_id), AMOUNT);
}

#[test]
fn dispute_blocked_after_inspection_window() {
    let s = setup();
    let id = s.create();
    s.escrow.mark_delivered(&id);
    s.advance(INSPECTION);

    assert_eq!(s.escrow.try_dispute(&id), Err(Ok(Error::InspectionOver)));
}

#[test]
fn dispute_blocked_before_delivery_marked() {
    let s = setup();
    let id = s.create();

    assert_eq!(s.escrow.try_dispute(&id), Err(Ok(Error::InvalidState)));
}

#[test]
fn arbiter_splits_disputed_deal() {
    // Scratched phone: 30% back to the buyer, 70% to the seller.
    let s = setup();
    let id = s.create();
    s.escrow.mark_delivered(&id);
    s.escrow.dispute(&id);
    s.escrow.resolve(&id, &300);

    assert_eq!(s.status(id), Status::Resolved);
    assert_eq!(s.balance(&s.buyer), AMOUNT * 9 + 300);
    assert_eq!(s.balance(&s.seller), 700);
    assert_eq!(s.balance(&s.escrow_id), 0);
    assert_eq!(s.balance(&s.arbiter), 0);
}

#[test]
fn arbiter_full_refund_and_full_release() {
    let s = setup();

    let a = s.create();
    s.escrow.mark_delivered(&a);
    s.escrow.dispute(&a);
    s.escrow.resolve(&a, &AMOUNT);

    let b = s.create();
    s.escrow.mark_delivered(&b);
    s.escrow.dispute(&b);
    s.escrow.resolve(&b, &0);

    assert_eq!(s.balance(&s.seller), AMOUNT);
    assert_eq!(s.balance(&s.buyer), AMOUNT * 9);
}

#[test]
fn arbiter_share_out_of_range_rejected() {
    let s = setup();
    let id = s.create();
    s.escrow.mark_delivered(&id);
    s.escrow.dispute(&id);

    assert_eq!(s.escrow.try_resolve(&id, &(AMOUNT + 1)), Err(Ok(Error::InvalidShare)));
    assert_eq!(s.escrow.try_resolve(&id, &-1), Err(Ok(Error::InvalidShare)));
}

#[test]
fn arbiter_cannot_touch_undisputed_deals() {
    let s = setup();
    let funded = s.create();
    let delivered = s.create();
    s.escrow.mark_delivered(&delivered);

    assert_eq!(s.escrow.try_resolve(&funded, &0), Err(Ok(Error::InvalidState)));
    assert_eq!(s.escrow.try_resolve(&delivered, &0), Err(Ok(Error::InvalidState)));
}

// ---------------------------------------------------------------- terminal states

#[test]
fn terminal_states_reject_every_action() {
    let s = setup();

    let released = s.create();
    s.escrow.confirm(&released);

    let refunded = s.create();
    let cancelled = s.create();
    s.escrow.cancel(&cancelled);
    s.advance(DELIVERY + 1);
    s.escrow.claim_refund(&refunded);

    let resolved = s.create();
    s.escrow.mark_delivered(&resolved);
    s.escrow.dispute(&resolved);
    s.escrow.resolve(&resolved, &500);

    for id in [released, refunded, cancelled, resolved] {
        let bad = Err(Ok(Error::InvalidState));
        assert_eq!(s.escrow.try_confirm(&id), bad);
        assert_eq!(s.escrow.try_mark_delivered(&id), bad);
        assert_eq!(s.escrow.try_dispute(&id), bad);
        assert_eq!(s.escrow.try_claim_release(&id), bad);
        assert_eq!(s.escrow.try_claim_refund(&id), bad);
        assert_eq!(s.escrow.try_cancel(&id), bad);
        assert_eq!(s.escrow.try_resolve(&id, &0), bad);
    }
    assert_eq!(s.balance(&s.escrow_id), 0);
}

#[test]
fn double_confirm_rejected() {
    let s = setup();
    let id = s.create();
    s.escrow.confirm(&id);

    assert_eq!(s.escrow.try_confirm(&id), Err(Ok(Error::InvalidState)));
    assert_eq!(s.balance(&s.seller), AMOUNT);
}

// ---------------------------------------------------------------- create validation

#[test]
fn create_rejects_bad_input() {
    let s = setup();
    let e = &s.escrow;
    let (b, sl, ar) = (&s.buyer, &s.seller, &s.arbiter);

    assert_eq!(e.try_create(b, sl, &0, &DELIVERY, &INSPECTION), Err(Ok(Error::InvalidAmount)));
    assert_eq!(e.try_create(b, sl, &-5, &DELIVERY, &INSPECTION), Err(Ok(Error::InvalidAmount)));
    assert_eq!(e.try_create(b, b, &AMOUNT, &DELIVERY, &INSPECTION), Err(Ok(Error::SameParty)));
    assert_eq!(e.try_create(b, ar, &AMOUNT, &DELIVERY, &INSPECTION), Err(Ok(Error::ArbiterIsParty)));
    assert_eq!(e.try_create(ar, sl, &AMOUNT, &DELIVERY, &INSPECTION), Err(Ok(Error::ArbiterIsParty)));
    assert_eq!(
        e.try_create(b, sl, &AMOUNT, &(MIN_WINDOW_SECS - 1), &INSPECTION),
        Err(Ok(Error::InvalidDeliveryWindow))
    );
    assert_eq!(
        e.try_create(b, sl, &AMOUNT, &(MAX_DELIVERY_SECS + 1), &INSPECTION),
        Err(Ok(Error::InvalidDeliveryWindow))
    );
    assert_eq!(
        e.try_create(b, sl, &AMOUNT, &DELIVERY, &(MIN_WINDOW_SECS - 1)),
        Err(Ok(Error::InvalidInspectionWindow))
    );
    assert_eq!(
        e.try_create(b, sl, &AMOUNT, &DELIVERY, &(MAX_INSPECTION_SECS + 1)),
        Err(Ok(Error::InvalidInspectionWindow))
    );
    assert_eq!(s.balance(&s.escrow_id), 0);
}

#[test]
fn unknown_deal_rejected() {
    let s = setup();
    assert_eq!(s.escrow.try_get_deal(&42), Err(Ok(Error::DealNotFound)));
    assert_eq!(s.escrow.try_confirm(&42), Err(Ok(Error::DealNotFound)));
}

// ---------------------------------------------------------------- who must sign

#[test]
fn confirm_requires_the_stored_buyer() {
    let s = setup();
    let id = s.create();
    s.escrow.confirm(&id);
    s.assert_signed_by(&s.buyer, "confirm", (id,).into_val(&s.env));
}

#[test]
fn dispute_requires_the_stored_buyer() {
    let s = setup();
    let id = s.create();
    s.escrow.mark_delivered(&id);
    s.escrow.dispute(&id);
    s.assert_signed_by(&s.buyer, "dispute", (id,).into_val(&s.env));
}

#[test]
fn mark_delivered_requires_the_stored_seller() {
    let s = setup();
    let id = s.create();
    s.escrow.mark_delivered(&id);
    s.assert_signed_by(&s.seller, "mark_delivered", (id,).into_val(&s.env));
}

#[test]
fn cancel_requires_the_stored_seller() {
    let s = setup();
    let id = s.create();
    s.escrow.cancel(&id);
    s.assert_signed_by(&s.seller, "cancel", (id,).into_val(&s.env));
}

#[test]
fn resolve_requires_the_arbiter() {
    let s = setup();
    let id = s.create();
    s.escrow.mark_delivered(&id);
    s.escrow.dispute(&id);
    s.escrow.resolve(&id, &100);
    s.assert_signed_by(&s.arbiter, "resolve", (id, 100_i128).into_val(&s.env));
}

#[test]
fn time_based_claims_need_no_signature() {
    let s = setup();
    let id = s.create();
    s.escrow.mark_delivered(&id);
    s.advance(INSPECTION);
    s.escrow.claim_release(&id);
    assert_eq!(s.env.auths(), std::vec![]);
}

#[test]
fn seller_cannot_confirm_their_own_sale() {
    let s = setup();
    let id = s.create();

    let result = s
        .escrow
        .mock_auths(&[MockAuth {
            address: &s.seller,
            invoke: &MockAuthInvoke {
                contract: &s.escrow_id,
                fn_name: "confirm",
                args: (id,).into_val(&s.env),
                sub_invokes: &[],
            },
        }])
        .try_confirm(&id);

    assert!(result.is_err());
    assert_eq!(s.status(id), Status::Funded);
    assert_eq!(s.balance(&s.seller), 0);
}

#[test]
fn stranger_cannot_cancel_and_refund_buyer() {
    let s = setup();
    let id = s.create();
    let stranger = Address::generate(&s.env);

    let result = s
        .escrow
        .mock_auths(&[MockAuth {
            address: &stranger,
            invoke: &MockAuthInvoke {
                contract: &s.escrow_id,
                fn_name: "cancel",
                args: (id,).into_val(&s.env),
                sub_invokes: &[],
            },
        }])
        .try_cancel(&id);

    assert!(result.is_err());
    assert_eq!(s.status(id), Status::Funded);
}

// ---------------------------------------------------------------- listing

#[test]
fn get_deals_pages_and_caps() {
    let s = setup();
    for _ in 0..3 {
        s.create();
    }

    let all = s.escrow.get_deals(&0, &10);
    assert_eq!(all.len(), 3);
    assert_eq!(all.get(2).unwrap().0, 2);

    let tail = s.escrow.get_deals(&2, &10);
    assert_eq!(tail.len(), 1);

    assert_eq!(s.escrow.get_deals(&5, &10).len(), 0);
}
