//! Bidazk escrow.
//!
//! A buyer locks payment for one deal with one seller. The money can only ever leave
//! the contract to that deal's buyer or that deal's seller. Time settles the deal when
//! either side goes silent; the arbiter settles it only when the buyer disputes.
//!
//! See DESIGN.md at the repo root for the full state machine and the loophole table.

#![no_std]

use soroban_sdk::{
    contract, contracterror, contractevent, contractimpl, contracttype, token, Address, Env, Vec,
};

/// Bounds on the windows a buyer may choose. The minimum is deliberately short so the
/// time-based paths can be demonstrated live; production would raise the inspection
/// minimum to at least 24 hours.
const MIN_WINDOW_SECS: u64 = 60;
const MAX_DELIVERY_SECS: u64 = 30 * 24 * 60 * 60;
const MAX_INSPECTION_SECS: u64 = 14 * 24 * 60 * 60;

/// Ledgers close roughly every 5 seconds.
const DAY_IN_LEDGERS: u32 = 17_280;
const TTL_EXTEND_TO: u32 = 60 * DAY_IN_LEDGERS;
const TTL_THRESHOLD: u32 = TTL_EXTEND_TO - DAY_IN_LEDGERS;

const MAX_PAGE: u32 = 50;

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum Error {
    InvalidAmount = 1,
    SameParty = 2,
    ArbiterIsParty = 3,
    InvalidDeliveryWindow = 4,
    InvalidInspectionWindow = 5,
    DealNotFound = 6,
    InvalidState = 7,
    DeliveryDeadlinePassed = 8,
    DeliveryDeadlineNotPassed = 9,
    InspectionOver = 10,
    InspectionNotOver = 11,
    InvalidShare = 12,
}

#[contracttype]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Status {
    /// Buyer has paid into escrow; seller has not marked delivery.
    Funded,
    /// Seller says the item was handed over; the inspection window is running.
    Delivered,
    /// Buyer rejected the delivery inside the window; only the arbiter can settle it.
    Disputed,
    /// Terminal: seller was paid in full.
    Released,
    /// Terminal: buyer was refunded because the seller missed the delivery deadline.
    Refunded,
    /// Terminal: seller cancelled before delivery; buyer was refunded.
    Cancelled,
    /// Terminal: arbiter split the money between buyer and seller.
    Resolved,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Deal {
    pub buyer: Address,
    pub seller: Address,
    pub amount: i128,
    pub status: Status,
    pub created_at: u64,
    pub delivery_deadline: u64,
    pub inspection_period: u64,
    /// Zero until the seller marks delivery.
    pub delivered_at: u64,
    /// What the buyer got back when the deal closed. The seller received `amount` minus this.
    pub buyer_refund: i128,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Config {
    pub arbiter: Address,
    pub token: Address,
}

#[contracttype]
#[derive(Clone)]
enum DataKey {
    Config,
    NextId,
    Deal(u64),
}

#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DealCreated {
    #[topic]
    pub deal_id: u64,
    pub buyer: Address,
    pub seller: Address,
    pub amount: i128,
}

#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StatusChanged {
    #[topic]
    pub deal_id: u64,
    pub status: Status,
    pub buyer_refund: i128,
}

#[contract]
pub struct Escrow;

#[contractimpl]
impl Escrow {
    /// The arbiter and the token are fixed for the life of the contract. Taking the token
    /// per deal would let a caller point the contract at a malicious token.
    pub fn __constructor(env: Env, arbiter: Address, token: Address) {
        env.storage().instance().set(&DataKey::Config, &Config { arbiter, token });
        env.storage().instance().set(&DataKey::NextId, &0u64);
    }

    /// Buyer locks `amount` for `seller`. The seller has `delivery_window` seconds to mark
    /// delivery; after that, the buyer has `inspection_period` seconds to dispute.
    pub fn create(
        env: Env,
        buyer: Address,
        seller: Address,
        amount: i128,
        delivery_window: u64,
        inspection_period: u64,
    ) -> Result<u64, Error> {
        buyer.require_auth();
        let config = Self::load_config(&env);

        if amount <= 0 {
            return Err(Error::InvalidAmount);
        }
        if buyer == seller {
            return Err(Error::SameParty);
        }
        if buyer == config.arbiter || seller == config.arbiter {
            return Err(Error::ArbiterIsParty);
        }
        if !(MIN_WINDOW_SECS..=MAX_DELIVERY_SECS).contains(&delivery_window) {
            return Err(Error::InvalidDeliveryWindow);
        }
        if !(MIN_WINDOW_SECS..=MAX_INSPECTION_SECS).contains(&inspection_period) {
            return Err(Error::InvalidInspectionWindow);
        }

        let now = env.ledger().timestamp();
        let id: u64 = env.storage().instance().get(&DataKey::NextId).unwrap_or(0);
        env.storage().instance().set(&DataKey::NextId, &(id + 1));

        let deal = Deal {
            buyer: buyer.clone(),
            seller: seller.clone(),
            amount,
            status: Status::Funded,
            created_at: now,
            delivery_deadline: now + delivery_window,
            inspection_period,
            delivered_at: 0,
            buyer_refund: 0,
        };
        Self::save_deal(&env, id, &deal);

        token::TokenClient::new(&env, &config.token).transfer(
            &buyer,
            &env.current_contract_address(),
            &amount,
        );

        DealCreated { deal_id: id, buyer, seller, amount }.publish(&env);
        Ok(id)
    }

    /// Seller declares the item handed over. Closes the buyer's refund path and starts the
    /// inspection window.
    pub fn mark_delivered(env: Env, deal_id: u64) -> Result<(), Error> {
        let mut deal = Self::load_deal(&env, deal_id)?;
        deal.seller.require_auth();
        Self::expect(&deal, &[Status::Funded])?;

        let now = env.ledger().timestamp();
        if now > deal.delivery_deadline {
            return Err(Error::DeliveryDeadlinePassed);
        }

        deal.delivered_at = now;
        Self::transition(&env, deal_id, &mut deal, Status::Delivered);
        Ok(())
    }

    /// Buyer is satisfied. Final: the seller is paid and nothing can reverse it.
    pub fn confirm(env: Env, deal_id: u64) -> Result<(), Error> {
        let mut deal = Self::load_deal(&env, deal_id)?;
        deal.buyer.require_auth();
        Self::expect(&deal, &[Status::Funded, Status::Delivered])?;

        Self::transition(&env, deal_id, &mut deal, Status::Released);
        Self::pay(&env, &deal.seller, deal.amount);
        Ok(())
    }

    /// Buyer rejects the delivery while the inspection window is open. Freezes the money
    /// until the arbiter resolves it.
    pub fn dispute(env: Env, deal_id: u64) -> Result<(), Error> {
        let mut deal = Self::load_deal(&env, deal_id)?;
        deal.buyer.require_auth();
        Self::expect(&deal, &[Status::Delivered])?;

        if env.ledger().timestamp() >= Self::inspection_end(&deal) {
            return Err(Error::InspectionOver);
        }

        Self::transition(&env, deal_id, &mut deal, Status::Disputed);
        Ok(())
    }

    /// Anyone may call: pays the seller once the inspection window closes without a
    /// dispute. Protects the seller from a buyer who goes silent.
    pub fn claim_release(env: Env, deal_id: u64) -> Result<(), Error> {
        let mut deal = Self::load_deal(&env, deal_id)?;
        Self::expect(&deal, &[Status::Delivered])?;

        if env.ledger().timestamp() < Self::inspection_end(&deal) {
            return Err(Error::InspectionNotOver);
        }

        Self::transition(&env, deal_id, &mut deal, Status::Released);
        Self::pay(&env, &deal.seller, deal.amount);
        Ok(())
    }

    /// Anyone may call: refunds the buyer once the delivery deadline passes with no
    /// delivery. Protects the buyer from a seller who goes silent.
    pub fn claim_refund(env: Env, deal_id: u64) -> Result<(), Error> {
        let mut deal = Self::load_deal(&env, deal_id)?;
        Self::expect(&deal, &[Status::Funded])?;

        if env.ledger().timestamp() <= deal.delivery_deadline {
            return Err(Error::DeliveryDeadlineNotPassed);
        }

        deal.buyer_refund = deal.amount;
        Self::transition(&env, deal_id, &mut deal, Status::Refunded);
        Self::pay(&env, &deal.buyer, deal.amount);
        Ok(())
    }

    /// Seller cannot fulfil (out of stock, wrong listing). Refunds the buyer immediately.
    pub fn cancel(env: Env, deal_id: u64) -> Result<(), Error> {
        let mut deal = Self::load_deal(&env, deal_id)?;
        deal.seller.require_auth();
        Self::expect(&deal, &[Status::Funded])?;

        deal.buyer_refund = deal.amount;
        Self::transition(&env, deal_id, &mut deal, Status::Cancelled);
        Self::pay(&env, &deal.buyer, deal.amount);
        Ok(())
    }

    /// Arbiter settles a dispute by splitting the money between this deal's buyer and
    /// seller. There is no way to send it anywhere else.
    pub fn resolve(env: Env, deal_id: u64, buyer_share: i128) -> Result<(), Error> {
        let mut deal = Self::load_deal(&env, deal_id)?;
        Self::load_config(&env).arbiter.require_auth();
        Self::expect(&deal, &[Status::Disputed])?;

        if buyer_share < 0 || buyer_share > deal.amount {
            return Err(Error::InvalidShare);
        }

        deal.buyer_refund = buyer_share;
        Self::transition(&env, deal_id, &mut deal, Status::Resolved);
        Self::pay(&env, &deal.buyer, buyer_share);
        Self::pay(&env, &deal.seller, deal.amount - buyer_share);
        Ok(())
    }

    pub fn get_deal(env: Env, deal_id: u64) -> Result<Deal, Error> {
        Self::load_deal(&env, deal_id)
    }

    /// Deals `start..start+limit`, newest last. Lets a client list deals in one call
    /// instead of one call per deal.
    pub fn get_deals(env: Env, start: u64, limit: u32) -> Vec<(u64, Deal)> {
        let count = Self::deal_count(env.clone());
        let end = count.min(start.saturating_add(limit.min(MAX_PAGE) as u64));
        let mut deals = Vec::new(&env);
        for id in start..end {
            if let Ok(deal) = Self::load_deal(&env, id) {
                deals.push_back((id, deal));
            }
        }
        deals
    }

    pub fn deal_count(env: Env) -> u64 {
        env.storage().instance().get(&DataKey::NextId).unwrap_or(0)
    }

    pub fn config(env: Env) -> Config {
        Self::load_config(&env)
    }
}

impl Escrow {
    fn load_config(env: &Env) -> Config {
        env.storage()
            .instance()
            .extend_ttl(TTL_THRESHOLD, TTL_EXTEND_TO);
        env.storage().instance().get(&DataKey::Config).unwrap()
    }

    fn load_deal(env: &Env, deal_id: u64) -> Result<Deal, Error> {
        env.storage()
            .persistent()
            .get(&DataKey::Deal(deal_id))
            .ok_or(Error::DealNotFound)
    }

    /// Every write extends the deal's TTL so a long-running deal is never archived.
    fn save_deal(env: &Env, deal_id: u64, deal: &Deal) {
        let key = DataKey::Deal(deal_id);
        env.storage().persistent().set(&key, deal);
        env.storage()
            .persistent()
            .extend_ttl(&key, TTL_THRESHOLD, TTL_EXTEND_TO);
    }

    fn expect(deal: &Deal, allowed: &[Status]) -> Result<(), Error> {
        if allowed.contains(&deal.status) {
            Ok(())
        } else {
            Err(Error::InvalidState)
        }
    }

    /// State is written before any money moves, so a deal can never pay out twice.
    fn transition(env: &Env, deal_id: u64, deal: &mut Deal, status: Status) {
        deal.status = status;
        Self::save_deal(env, deal_id, deal);
        StatusChanged { deal_id, status, buyer_refund: deal.buyer_refund }.publish(env);
    }

    fn inspection_end(deal: &Deal) -> u64 {
        deal.delivered_at + deal.inspection_period
    }

    fn pay(env: &Env, to: &Address, amount: i128) {
        if amount > 0 {
            let token = Self::load_config(env).token;
            token::TokenClient::new(env, &token).transfer(
                &env.current_contract_address(),
                to,
                &amount,
            );
        }
    }
}

mod test;
