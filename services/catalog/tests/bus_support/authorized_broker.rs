//! A `nats-server` child with authorization on, built from the reviewed GITHUB fragment.
//!
//! The same broker serves the authorization test and the relay health test (XR-021 CONTRACTS.md
//! S03 and R2-03), so what both prove is the deployed ACL.

#![allow(
    dead_code,
    unreachable_pub,
    missing_docs,
    clippy::expect_used,
    clippy::unwrap_used,
    reason = "each test binary compiles this shared module and uses a different subset"
)]

use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::time::Duration;

use uuid::Uuid;

const FRAGMENT: &str = "deploy/nats/identity.conf";
const PLACEHOLDER_PREFIX: &str = "UREPLACE_ME_WITH_THE_PUBLIC_NKEY_OF_RATATOSKR_GITHUB_";

/// A `nats-server` child with authorization on, killed when the test ends.
pub struct AuthorizedBroker {
    child: Child,
    port: u16,
    pub admin_seed: PathBuf,
    pub github_seed: PathBuf,
    _directory: tempdir::Directory,
}

mod tempdir {
    //! A throwaway directory under the cargo target directory, removed on drop.
    use std::path::{Path, PathBuf};

    #[derive(Debug)]
    pub(super) struct Directory(PathBuf);

    impl Directory {
        pub(super) fn create(name: &str) -> std::io::Result<Self> {
            let path = Path::new(env!("CARGO_TARGET_TMPDIR")).join(name);
            std::fs::create_dir_all(&path)?;
            Ok(Self(path))
        }

        pub(super) fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for Directory {
        fn drop(&mut self) {
            let _removed = std::fs::remove_dir_all(&self.0);
        }
    }
}

/// What a test changes about the reviewed broker.
#[derive(Debug, Default)]
pub struct BrokerOptions {
    /// A publish subject removed from the GITHUB stanza, to model an ACL that drifted.
    pub omit_publish: Option<String>,
    /// The broker's `max_payload` in bytes, when it is not the default.
    pub max_payload: Option<u32>,
}

impl Drop for AuthorizedBroker {
    fn drop(&mut self) {
        let _killed = self.child.kill();
        let _reaped = self.child.wait();
    }
}

/// Held from picking a port until the broker listens on it. Tests in one binary run in parallel,
/// and a port is only taken once its server is listening, so without this two brokers pick the
/// same port, the second `nats-server` exits on the bind error, and its test connects to the
/// first test's broker with the wrong identity (an authorization violation).
static START: Mutex<()> = Mutex::new(());

/// A free port in the range reserved for this repository's private test servers.
fn free_port() -> Result<u16, Box<dyn std::error::Error>> {
    (57_710..=57_719)
        .find(|port| TcpListener::bind(("127.0.0.1", *port)).is_ok())
        .ok_or_else(|| "no free port in 57710..=57719".into())
}

fn write_seed(
    directory: &Path,
    name: &str,
    pair: &nkeys::KeyPair,
) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let path = directory.join(name);
    std::fs::write(&path, pair.seed()?)?;
    Ok(path)
}

/// The reviewed fragment with its placeholder swapped for the test identity's public key.
fn github_stanza(
    public_key: &str,
    omit_publish: Option<&str>,
) -> Result<String, Box<dyn std::error::Error>> {
    let fragment = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join(FRAGMENT),
    )?;
    assert!(
        fragment.contains(PLACEHOLDER_PREFIX),
        "the fragment must carry the GITHUB placeholder nkey"
    );
    Ok(fragment
        .lines()
        .filter(|line| omit_publish.is_none_or(|subject| !line.contains(&format!("\"{subject}\""))))
        .map(|line| {
            if line.trim_start().starts_with("nkey:") {
                format!("nkey: {public_key}")
            } else {
                line.to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join("\n"))
}

impl AuthorizedBroker {
    /// A broker with the whole reviewed fragment and the default payload limit.
    pub fn start() -> Result<Self, Box<dyn std::error::Error>> {
        Self::start_with(&BrokerOptions::default())
    }

    pub fn start_with(options: &BrokerOptions) -> Result<Self, Box<dyn std::error::Error>> {
        let directory = tempdir::Directory::create(&format!("authorized-bus-{}", Uuid::now_v7()))?;
        let _starting = START
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let port = free_port()?;
        let admin = nkeys::KeyPair::new_user();
        let github = nkeys::KeyPair::new_user();
        let admin_seed = write_seed(directory.path(), "admin.nkey", &admin)?;
        let github_seed = write_seed(directory.path(), "github.nkey", &github)?;
        let max_payload = options
            .max_payload
            .map(|bytes| format!("max_payload: {bytes}\n"))
            .unwrap_or_default();
        let configuration = format!(
            "port: {port}\nhost: 127.0.0.1\n{max_payload}jetstream {{ store_dir: \"{store}\" }}\n\
             authorization {{ users: [\n\
             {{ nkey: {admin_key}, permissions: {{ publish: {{ allow: [\">\"] }}, subscribe: {{ allow: [\">\"] }} }} }},\n\
             {stanza}\n] }}\n",
            store = directory.path().join("store").display(),
            admin_key = admin.public_key(),
            stanza = github_stanza(&github.public_key(), options.omit_publish.as_deref())?,
        );
        let configuration_path = directory.path().join("broker.conf");
        std::fs::write(&configuration_path, configuration)?;
        let child = Command::new("nats-server")
            .arg("-c")
            .arg(&configuration_path)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;
        let broker = Self {
            child,
            port,
            admin_seed,
            github_seed,
            _directory: directory,
        };
        broker.wait_until_listening()?;
        Ok(broker)
    }

    fn wait_until_listening(&self) -> Result<(), Box<dyn std::error::Error>> {
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while std::net::TcpStream::connect(("127.0.0.1", self.port)).is_err() {
            if std::time::Instant::now() >= deadline {
                return Err("the authorized broker did not start listening".into());
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        Ok(())
    }

    pub fn url(&self) -> String {
        format!("nats://127.0.0.1:{}", self.port)
    }

    pub async fn connect(
        &self,
        seed: &Path,
    ) -> Result<async_nats::Client, Box<dyn std::error::Error>> {
        let seed = std::fs::read_to_string(seed)?;
        Ok(
            async_nats::ConnectOptions::with_nkey(seed.trim().to_owned())
                .connect(self.url())
                .await?,
        )
    }
}
