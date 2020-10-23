use serum_common::pack::Pack;
use serum_registry::access_control;
use serum_registry::accounts::{vault, Entity, Member, Registrar};
use serum_registry::error::{RegistryError, RegistryErrorCode};
use solana_sdk::account_info::{next_account_info, AccountInfo};
use solana_sdk::info;
use solana_sdk::pubkey::Pubkey;
use solana_sdk::sysvar::clock::Clock;

pub fn handler<'a>(
    program_id: &'a Pubkey,
    accounts: &'a [AccountInfo<'a>],
    amount: u64,
    is_mega: bool,
    is_delegate: bool,
) -> Result<(), RegistryError> {
    info!("handler: stake_intent");

    let acc_infos = &mut accounts.iter();

    // Lockup whitelist relay interface.

    let delegate_owner_acc_info = next_account_info(acc_infos)?;
    let depositor_tok_acc_info = next_account_info(acc_infos)?;
    let vault_acc_info = next_account_info(acc_infos)?;
    // Owner or token delegate.
    let tok_authority_acc_info = next_account_info(acc_infos)?;
    let token_program_acc_info = next_account_info(acc_infos)?;

    // Program specfic.

    let member_acc_info = next_account_info(acc_infos)?;
    let beneficiary_acc_info = next_account_info(acc_infos)?;
    let entity_acc_info = next_account_info(acc_infos)?;
    let registrar_acc_info = next_account_info(acc_infos)?;
    let clock_acc_info = next_account_info(acc_infos)?;

    access_control(AccessControlRequest {
        delegate_owner_acc_info,
        tok_authority_acc_info,
        depositor_tok_acc_info,
        member_acc_info,
        beneficiary_acc_info,
        entity_acc_info,
        vault_acc_info,
        token_program_acc_info,
        is_delegate,
        is_mega,
        program_id,
        registrar_acc_info,
    })?;

    Entity::unpack_mut(
        &mut entity_acc_info.try_borrow_mut_data()?,
        &mut |entity: &mut Entity| {
            Member::unpack_mut(
                &mut member_acc_info.try_borrow_mut_data()?,
                &mut |member: &mut Member| {
                    let clock = access_control::clock(clock_acc_info)?;
                    let registrar = Registrar::unpack(&registrar_acc_info.try_borrow_data()?)?;
                    state_transition(StateTransitionRequest {
                        entity,
                        member,
                        amount,
                        registrar,
                        clock,
                        registrar_acc_info,
                        vault_acc_info,
                        tok_authority_acc_info,
                        depositor_tok_acc_info,
                        member_acc_info,
                        entity_acc_info,
                        token_program_acc_info,
                        is_delegate,
                        is_mega,
                    })
                    .map_err(Into::into)
                },
            )
        },
    )?;

    Ok(())
}

fn access_control(req: AccessControlRequest) -> Result<(), RegistryError> {
    info!("access-control: stake_intent");

    let AccessControlRequest {
        delegate_owner_acc_info,
        tok_authority_acc_info,
        depositor_tok_acc_info,
        member_acc_info,
        beneficiary_acc_info,
        entity_acc_info,
        vault_acc_info,
        token_program_acc_info,
        registrar_acc_info,
        program_id,
        is_delegate,
        is_mega,
    } = req;

    // Beneficiary authorization.
    if !beneficiary_acc_info.is_signer {
        return Err(RegistryErrorCode::Unauthorized)?;
    }
    // Delegate implies the signer is the program-derived-address of the
    // lockup program, the vault of which we have delegate access to.
    if is_delegate {
        if !delegate_owner_acc_info.is_signer {
            return Err(RegistryErrorCode::Unauthorized)?;
        }
    }
    // No delegate implies it's a regular transfer and so the owner must sign.
    else {
        if !tok_authority_acc_info.is_signer {
            return Err(RegistryErrorCode::Unauthorized)?;
        }
    }

    // Account validation.
    let registrar = access_control::registrar(registrar_acc_info, program_id)?;
    let _ = access_control::entity(entity_acc_info, registrar_acc_info, program_id)?;
    let member = access_control::member(
        member_acc_info,
        entity_acc_info,
        beneficiary_acc_info,
        Some(delegate_owner_acc_info),
        is_delegate,
        program_id,
    )?;
    let vault = access_control::vault(
        vault_acc_info,
        registrar_acc_info,
        &registrar,
        program_id,
        is_mega,
    )?;
    if is_delegate {
        // Match the signer to the Member account's delegate.
        if *delegate_owner_acc_info.key != member.books.delegate().owner {
            return Err(RegistryErrorCode::InvalidMemberDelegateOwner)?;
        }
        // tok_authority is this vault's owner because this is a delegate
        // transfer.
        if *tok_authority_acc_info.key != vault.owner {
            return Err(RegistryErrorCode::InvalidTokenAuthority)?;
        }
    }

    // StakeIntent specific: None.

    info!("access-control: success");

    Ok(())
}

fn state_transition(req: StateTransitionRequest) -> Result<(), RegistryError> {
    info!("state-transition: stake_intent");

    let StateTransitionRequest {
        entity,
        member,
        amount,
        registrar,
        clock,
        registrar_acc_info,
        tok_authority_acc_info,
        depositor_tok_acc_info,
        vault_acc_info,
        member_acc_info,
        entity_acc_info,
        token_program_acc_info,
        is_delegate,
        is_mega,
    } = req;

    // Transfer funds into the stake intent vault.
    {
        info!("invoking token transfer");
        let withdraw_instruction = spl_token::instruction::transfer(
            &spl_token::ID,
            depositor_tok_acc_info.key,
            vault_acc_info.key,
            tok_authority_acc_info.key,
            &[],
            amount,
        )?;
        let acc_infos = &[
            depositor_tok_acc_info.clone(),
            vault_acc_info.clone(),
            tok_authority_acc_info.clone(),
            token_program_acc_info.clone(),
        ];
        if is_delegate {
            solana_sdk::program::invoke_signed(
                &withdraw_instruction,
                acc_infos,
                &[&vault::signer_seeds(
                    registrar_acc_info.key,
                    &registrar.nonce,
                )],
            )?;
        } else {
            solana_sdk::program::invoke_signed(&withdraw_instruction, acc_infos, &[])?;
        }
    }

    member.add_stake_intent(amount, is_mega, is_delegate);
    entity.add_stake_intent(amount, is_mega, &registrar, &clock);

    info!("state-transition: success");

    Ok(())
}

struct AccessControlRequest<'a> {
    delegate_owner_acc_info: &'a AccountInfo<'a>,
    registrar_acc_info: &'a AccountInfo<'a>,
    program_id: &'a Pubkey,
    tok_authority_acc_info: &'a AccountInfo<'a>,
    depositor_tok_acc_info: &'a AccountInfo<'a>,
    member_acc_info: &'a AccountInfo<'a>,
    beneficiary_acc_info: &'a AccountInfo<'a>,
    entity_acc_info: &'a AccountInfo<'a>,
    token_program_acc_info: &'a AccountInfo<'a>,
    vault_acc_info: &'a AccountInfo<'a>,
    is_delegate: bool,
    is_mega: bool,
}

struct StateTransitionRequest<'a, 'b> {
    entity: &'b mut Entity,
    member: &'b mut Member,
    is_mega: bool,
    is_delegate: bool,
    registrar: Registrar,
    clock: Clock,
    amount: u64,
    registrar_acc_info: &'a AccountInfo<'a>,
    vault_acc_info: &'a AccountInfo<'a>,
    tok_authority_acc_info: &'a AccountInfo<'a>,
    depositor_tok_acc_info: &'a AccountInfo<'a>,
    member_acc_info: &'a AccountInfo<'a>,
    entity_acc_info: &'a AccountInfo<'a>,
    token_program_acc_info: &'a AccountInfo<'a>,
}