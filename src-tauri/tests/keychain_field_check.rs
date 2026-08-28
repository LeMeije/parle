//! A DIAGNOSTIC, not a unit test. Run it deliberately:
//!
//! ```text
//! cargo test -p parle --test keychain_field_check -- --ignored --nocapture
//! ```
//!
//! `#[ignore]` because it writes to, reads from and deletes an item in the
//! REAL login keychain. It cleans up after itself, under a service name that is
//! not the app's, but it is still a side effect on the user's machine and does
//! not belong in the ordinary suite.
//!
//! Why it exists: `docs/SYNC_HANDOVER.md` lists the macOS keychain as an
//! unknown. On Windows this is Credential Manager and is silent; on macOS it is
//! Keychain Services, which can prompt, possibly on every access, and ACLs
//! items to the signing identity that wrote them, so a rebuilt unsigned binary
//! can be refused access to items an earlier build stored. A denied prompt
//! surfaces in the app as "this device is not paired", which is the most
//! confusing possible symptom.
//!
//! What this answers, on this machine, before anyone pairs anything:
//!   - can we store, read back and delete a secret at all;
//!   - does reading it back a second time prompt again;
//!   - is a missing entry reported as absent rather than as an error, which is
//!     what `keystore::load` relies on to mean "not paired".

use std::time::Instant;

const SERVICE: &str = "Parle sync FIELD CHECK";
const ACCOUNT: &str = "11111111-1111-4111-8111-111111111111";

fn entry() -> keyring::Entry {
    keyring::Entry::new(SERVICE, ACCOUNT).expect("could not address the credential store")
}

#[test]
#[ignore = "diagnostic: writes to the real login keychain; run deliberately"]
fn keychain_stores_reads_and_forgets_a_secret() {
    // Start clean, whatever a previous run left behind.
    let _ = entry().delete_credential();

    // 1. Absent must read as ABSENT, not as an error. `keystore::load` maps
    //    NoEntry to Ok(None) and everything else to a hard failure, so if this
    //    machine reports something else for a missing item, every unpaired
    //    device would look like a broken keychain.
    match entry().get_password() {
        Err(keyring::Error::NoEntry) => {}
        Ok(_) => panic!("a credential we just deleted is still readable"),
        Err(e) => panic!(
            "a MISSING credential reported {e:?} rather than NoEntry. \
             keystore::load treats that as a hard backend failure, so every \
             unpaired device would surface as a broken credential store."
        ),
    }

    let secret = "a".repeat(64);
    entry().set_password(&secret).expect("could not write to the credential store");

    // 2. Read it back, and time it. A prompt makes this take seconds and needs
    //    a human; silent access is milliseconds.
    let t = Instant::now();
    let got = entry().get_password().expect("could not read back what we just wrote");
    let first = t.elapsed();
    assert_eq!(got, secret, "the credential store returned something other than what we stored");

    // 3. And again. macOS can prompt per-access rather than once; sync reads a
    //    key on every dial, so a per-access prompt is a prompt storm.
    let t = Instant::now();
    let again = entry().get_password().expect("second read failed");
    let second = t.elapsed();
    assert_eq!(again, secret);

    println!("first read: {first:?}, second read: {second:?}");
    println!(
        "if either took more than a second, this machine is prompting per access; \
         sync reads a paired key on every dial, so that is a prompt storm and the \
         Always Allow button is the fix."
    );

    // 4. Unpairing must actually destroy it.
    entry().delete_credential().expect("could not delete the credential");
    assert!(
        matches!(entry().get_password(), Err(keyring::Error::NoEntry)),
        "a deleted credential is still readable: unpairing would not destroy the key"
    );
}
