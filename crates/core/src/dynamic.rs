//! Dynamic variables: `{{$name}}` generates fake data at send time
//! (AC-ENV-8 .. AC-ENV-10). Names and output shapes follow Postman's
//! dynamic variables so collections move between the two unchanged.
//!
//! * A name starting with `$` is never looked up in an environment
//!   (environment variable names may not start with `$`).
//! * One value per name per send: every resolve call made while preparing a
//!   request shares one [`DynamicCtx`], so the URL that is sent, the URL kept
//!   in history and two `{{$uuid}}` in the same request all agree.
//! * An unknown `$name` stays literal and is reported as unresolved.
//!
//! The UI mock keeps the same catalogue in `ui/src/lib/util/dynamic.ts`.

use rand::RngExt as _;
use rand::seq::IndexedRandom as _;
use serde::Serialize;
use std::collections::HashMap;
use std::sync::Mutex;

/// One catalogue entry, as returned by `get_dynamic_variables`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DynamicVariableInfo {
    /// Including the leading `$`, e.g. `$randomEmail`.
    pub name: String,
    pub description: String,
    pub example: String,
}

struct Generator {
    name: &'static str,
    description: &'static str,
    example: &'static str,
    generate: fn() -> String,
}

macro_rules! catalogue {
    ($( $name:literal, $desc:literal, $example:literal, $f:expr; )*) => {
        const CATALOGUE: &[Generator] = &[
            $( Generator { name: $name, description: $desc, example: $example, generate: $f }, )*
        ];
    };
}

catalogue! {
    "$guid", "A v4 UUID", "611c2e81-2ccb-42d8-9ddc-2d0bfa65c1b4", uuid_v4;
    "$uuid", "A v4 UUID", "3e6a4f26-8e0b-4b1c-9a35-0f1d7f0c2a9e", uuid_v4;
    "$randomUUID", "A v4 UUID", "6929bb52-3ab2-448a-9796-d6480ecad36b", uuid_v4;
    "$timestamp", "Current Unix time in seconds", "1795773600", timestamp;
    "$isoTimestamp", "Current time, ISO 8601 (UTC)", "2026-11-27T10:00:00.000Z", iso_timestamp;
    "$randomInt", "Random integer 0–1000", "482", random_int;
    "$randomBoolean", "Random true or false", "true", random_boolean;
    "$randomAlphaNumeric", "Random letter or digit", "k", random_alpha_numeric;
    "$randomPassword", "Random 15-character password", "t9iXe7COoDKv8k3", random_password;
    "$randomFirstName", "Random first name", "Jane", random_first_name;
    "$randomLastName", "Random last name", "Doe", random_last_name;
    "$randomFullName", "Random full name", "Jane Doe", random_full_name;
    "$randomUserName", "Random user name", "jane.doe42", random_user_name;
    "$randomEmail", "Random email", "jane.doe@example.com", random_email;
    "$randomPhoneNumber", "Random phone number", "555-201-7788", random_phone_number;
    "$randomCity", "Random city", "Lisbon", random_city;
    "$randomCountry", "Random country", "Portugal", random_country;
    "$randomCountryCode", "Random ISO 3166 country code", "PT", random_country_code;
    "$randomStreetName", "Random street name", "Maple Avenue", random_street_name;
    "$randomStreetAddress", "Random street address", "742 Maple Avenue", random_street_address;
    "$randomLatitude", "Random latitude", "38.722252", random_latitude;
    "$randomLongitude", "Random longitude", "-9.139337", random_longitude;
    "$randomCompanyName", "Random company name", "Northwind Labs", random_company_name;
    "$randomJobTitle", "Random job title", "Senior Data Engineer", random_job_title;
    "$randomProductName", "Random product name", "Ergonomic Steel Chair", random_product_name;
    "$randomPrice", "Random price 1.00–1000.00", "249.99", random_price;
    "$randomCurrencyCode", "Random ISO 4217 currency code", "EUR", random_currency_code;
    "$randomCreditCardMask", "Random last four card digits", "4821", random_credit_card_mask;
    "$randomUrl", "Random URL", "https://bright-harbor.example.com", random_url;
    "$randomDomainName", "Random domain name", "bright-harbor.example.com", random_domain_name;
    "$randomIP", "Random IPv4 address", "203.0.113.42", random_ip;
    "$randomIPV6", "Random IPv6 address", "2001:db8:85a3:8d3:1319:8a2e:370:7348", random_ipv6;
    "$randomMACAddress", "Random MAC address", "3c:22:fb:9e:41:07", random_mac_address;
    "$randomImageUrl", "Random 640×480 image URL", "https://picsum.photos/seed/4821/640/480", random_image_url;
    "$randomColor", "Random colour name", "teal", random_color;
    "$randomHexColor", "Random hex colour", "#1f9ede", random_hex_color;
    "$randomWord", "Random word", "harbor", random_word;
    "$randomWords", "Random words", "quiet amber river", random_words;
    "$randomLoremSentence", "Random lorem ipsum sentence", "Dolor sit amet consectetur.", random_lorem_sentence;
    "$randomLoremParagraph", "Random lorem ipsum paragraph", "Lorem ipsum dolor sit amet. Sed do eiusmod tempor. Ut enim ad minim veniam.", random_lorem_paragraph;
    "$randomDateFuture", "Random date in the next year, ISO 8601", "2027-03-14T08:21:37.000Z", random_date_future;
    "$randomDatePast", "Random date in the last year, ISO 8601", "2026-02-03T17:45:12.000Z", random_date_past;
    "$randomDateRecent", "Random date in the last day, ISO 8601", "2026-11-26T22:10:05.000Z", random_date_recent;
    "$randomSemver", "Random semantic version", "2.14.3", random_semver;
}

/// The catalogue, in display order.
pub fn catalogue() -> Vec<DynamicVariableInfo> {
    CATALOGUE
        .iter()
        .map(|g| DynamicVariableInfo {
            name: g.name.to_string(),
            description: g.description.to_string(),
            example: g.example.to_string(),
        })
        .collect()
}

/// Whether `name` (with its `$`) is a known dynamic variable.
pub fn is_known(name: &str) -> bool {
    CATALOGUE.iter().any(|g| g.name == name)
}

/// A fresh value for `name`, or `None` when the name is unknown.
pub fn generate(name: &str) -> Option<String> {
    CATALOGUE
        .iter()
        .find(|g| g.name == name)
        .map(|g| (g.generate)())
}

/// Per-send memo: one value per name for every resolve call that shares it.
#[derive(Debug, Default)]
pub struct DynamicCtx {
    keep: bool,
    memo: Mutex<HashMap<String, String>>,
}

impl DynamicCtx {
    /// Generate values (memoized per name).
    pub fn new() -> Self {
        DynamicCtx::default()
    }

    /// Leave `{{$name}}` literal (cURL export with "Keep {{variables}}").
    pub fn keep() -> Self {
        DynamicCtx {
            keep: true,
            memo: Mutex::default(),
        }
    }

    pub fn keeps(&self) -> bool {
        self.keep
    }

    /// The value for `name` in this send, or `None` when it is unknown.
    pub fn value(&self, name: &str) -> Option<String> {
        let mut memo = self.memo.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(v) = memo.get(name) {
            return Some(v.clone());
        }
        let v = generate(name)?;
        memo.insert(name.to_string(), v.clone());
        Some(v)
    }
}

// ------------------------------------------------------------ word lists

const FIRST_NAMES: &[&str] = &[
    "Ada", "Alan", "Amara", "Ben", "Carla", "Chen", "Diego", "Elena", "Emma", "Farah", "Grace",
    "Hana", "Ivan", "Jane", "Jonas", "Kai", "Lena", "Liam", "Maya", "Mateo", "Nia", "Noah",
    "Olivia", "Omar", "Priya", "Rosa", "Sam", "Sofia", "Tariq", "Yuki", "Zoe",
];
const LAST_NAMES: &[&str] = &[
    "Anderson", "Brown", "Costa", "Dubois", "Evans", "Fischer", "Garcia", "Hansen", "Ito",
    "Jensen", "Kowalski", "Lee", "Martin", "Nakamura", "Novak", "Okafor", "Patel", "Quinn",
    "Rossi", "Schmidt", "Silva", "Smith", "Tanaka", "Walker", "Weber", "Young", "Zhang",
];
const CITIES: &[&str] = &[
    "Amsterdam",
    "Austin",
    "Barcelona",
    "Berlin",
    "Bogotá",
    "Cape Town",
    "Chicago",
    "Dublin",
    "Helsinki",
    "Kyoto",
    "Lagos",
    "Lisbon",
    "London",
    "Melbourne",
    "Montreal",
    "Mumbai",
    "Nairobi",
    "Osaka",
    "Oslo",
    "Paris",
    "Prague",
    "Seoul",
    "Singapore",
    "Toronto",
    "Vienna",
    "Warsaw",
];
const COUNTRIES: &[(&str, &str)] = &[
    ("Argentina", "AR"),
    ("Australia", "AU"),
    ("Austria", "AT"),
    ("Brazil", "BR"),
    ("Canada", "CA"),
    ("Chile", "CL"),
    ("Denmark", "DK"),
    ("Egypt", "EG"),
    ("Finland", "FI"),
    ("France", "FR"),
    ("Germany", "DE"),
    ("Ghana", "GH"),
    ("India", "IN"),
    ("Ireland", "IE"),
    ("Italy", "IT"),
    ("Japan", "JP"),
    ("Kenya", "KE"),
    ("Mexico", "MX"),
    ("Netherlands", "NL"),
    ("New Zealand", "NZ"),
    ("Norway", "NO"),
    ("Poland", "PL"),
    ("Portugal", "PT"),
    ("Serbia", "RS"),
    ("South Korea", "KR"),
    ("Spain", "ES"),
    ("Sweden", "SE"),
    ("United Kingdom", "GB"),
    ("United States", "US"),
];
const STREET_NAMES: &[&str] = &[
    "Maple", "Oak", "Cedar", "Elm", "Harbor", "Hill", "Lake", "Park", "River", "Station", "Sunset",
    "Willow", "Church", "Mill", "Bridge", "Spring", "Forest", "Garden",
];
const STREET_SUFFIXES: &[&str] = &[
    "Street", "Avenue", "Road", "Lane", "Drive", "Way", "Court", "Place",
];
const COMPANY_WORDS: &[&str] = &[
    "Northwind",
    "Bluefin",
    "Brightside",
    "Cobalt",
    "Evergreen",
    "Granite",
    "Horizon",
    "Ironwood",
    "Juniper",
    "Lighthouse",
    "Meridian",
    "Nimbus",
    "Orchid",
    "Pinnacle",
    "Quartz",
    "Redwood",
    "Silverline",
    "Summit",
    "Tidal",
    "Vertex",
];
const COMPANY_SUFFIXES: &[&str] = &[
    "Labs", "Inc", "LLC", "Group", "Systems", "Partners", "Co", "Studios", "Works",
];
const JOB_LEVELS: &[&str] = &[
    "Junior",
    "Senior",
    "Lead",
    "Principal",
    "Chief",
    "Staff",
    "Associate",
];
const JOB_AREAS: &[&str] = &[
    "Data",
    "Product",
    "Security",
    "Marketing",
    "Platform",
    "Design",
    "Finance",
    "Operations",
    "Mobile",
    "Research",
    "Support",
    "Infrastructure",
];
const JOB_TYPES: &[&str] = &[
    "Engineer",
    "Designer",
    "Manager",
    "Analyst",
    "Architect",
    "Consultant",
    "Developer",
    "Specialist",
    "Strategist",
    "Coordinator",
];
const PRODUCT_ADJECTIVES: &[&str] = &[
    "Ergonomic",
    "Handcrafted",
    "Rustic",
    "Sleek",
    "Practical",
    "Refined",
    "Compact",
    "Durable",
    "Elegant",
    "Gorgeous",
    "Intelligent",
    "Modern",
    "Recycled",
    "Smart",
];
const PRODUCT_MATERIALS: &[&str] = &[
    "Steel", "Wooden", "Cotton", "Granite", "Plastic", "Rubber", "Bamboo", "Concrete", "Leather",
    "Glass", "Bronze", "Ceramic",
];
const PRODUCTS: &[&str] = &[
    "Chair",
    "Table",
    "Lamp",
    "Keyboard",
    "Mug",
    "Backpack",
    "Bottle",
    "Clock",
    "Desk",
    "Jacket",
    "Shoes",
    "Speaker",
    "Wallet",
    "Watch",
    "Headphones",
];
const CURRENCIES: &[&str] = &[
    "USD", "EUR", "GBP", "JPY", "CHF", "CAD", "AUD", "NZD", "SEK", "NOK", "DKK", "PLN", "CZK",
    "RSD", "BRL", "MXN", "INR", "CNY", "KRW", "SGD", "ZAR",
];
const COLORS: &[&str] = &[
    "red", "orange", "yellow", "green", "teal", "cyan", "blue", "indigo", "violet", "purple",
    "magenta", "pink", "brown", "black", "white", "gray", "silver", "gold", "olive", "maroon",
    "navy", "lime", "coral", "salmon",
];
const WORDS: &[&str] = &[
    "amber", "anchor", "arrow", "aurora", "beacon", "bloom", "breeze", "canyon", "cascade",
    "cedar", "comet", "coral", "crystal", "delta", "ember", "falcon", "fern", "glacier", "harbor",
    "island", "jade", "lagoon", "lantern", "maple", "meadow", "nebula", "ocean", "orbit", "pebble",
    "prairie", "quartz", "quiet", "river", "sable", "signal", "summit", "tide", "velvet", "willow",
    "zephyr",
];
const LOREM: &[&str] = &[
    "lorem",
    "ipsum",
    "dolor",
    "sit",
    "amet",
    "consectetur",
    "adipiscing",
    "elit",
    "sed",
    "do",
    "eiusmod",
    "tempor",
    "incididunt",
    "ut",
    "labore",
    "et",
    "dolore",
    "magna",
    "aliqua",
    "enim",
    "ad",
    "minim",
    "veniam",
    "quis",
    "nostrud",
    "exercitation",
    "ullamco",
    "laboris",
    "nisi",
    "aliquip",
    "ex",
    "ea",
    "commodo",
    "consequat",
    "duis",
    "aute",
    "irure",
    "in",
    "reprehenderit",
    "voluptate",
    "velit",
    "esse",
    "cillum",
    "fugiat",
    "nulla",
    "pariatur",
];
const EMAIL_DOMAINS: &[&str] = &["example.com", "example.org", "example.net"];
const ALNUM: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";

// ------------------------------------------------------------ generators

fn pick(list: &'static [&'static str]) -> &'static str {
    list.choose(&mut rand::rng()).copied().unwrap_or_default()
}

fn uuid_v4() -> String {
    uuid::Uuid::new_v4().to_string()
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// `ms` since the Unix epoch as `YYYY-MM-DDTHH:MM:SS.mmmZ`.
pub fn iso_from_millis(ms: i64) -> String {
    let days = ms.div_euclid(86_400_000);
    let rem = ms.rem_euclid(86_400_000);
    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.{:03}Z",
        rem / 3_600_000,
        rem / 60_000 % 60,
        rem / 1000 % 60,
        rem % 1000
    )
}

fn timestamp() -> String {
    (now_ms() / 1000).to_string()
}

fn iso_timestamp() -> String {
    iso_from_millis(now_ms())
}

fn random_int() -> String {
    rand::rng().random_range(0..=1000).to_string()
}

fn random_boolean() -> String {
    rand::rng().random_bool(0.5).to_string()
}

fn alnum(len: usize) -> String {
    let mut rng = rand::rng();
    (0..len)
        .map(|_| ALNUM[rng.random_range(0..ALNUM.len())] as char)
        .collect()
}

fn random_alpha_numeric() -> String {
    alnum(1)
}

fn random_password() -> String {
    alnum(15)
}

fn random_first_name() -> String {
    pick(FIRST_NAMES).to_string()
}

fn random_last_name() -> String {
    pick(LAST_NAMES).to_string()
}

fn random_full_name() -> String {
    format!("{} {}", pick(FIRST_NAMES), pick(LAST_NAMES))
}

fn random_user_name() -> String {
    format!(
        "{}.{}{}",
        pick(FIRST_NAMES).to_lowercase(),
        pick(LAST_NAMES).to_lowercase(),
        rand::rng().random_range(1..100)
    )
}

fn random_email() -> String {
    format!(
        "{}.{}@{}",
        pick(FIRST_NAMES).to_lowercase(),
        pick(LAST_NAMES).to_lowercase(),
        pick(EMAIL_DOMAINS)
    )
}

fn random_phone_number() -> String {
    let mut rng = rand::rng();
    format!(
        "555-{:03}-{:04}",
        rng.random_range(200..1000),
        rng.random_range(0..10_000)
    )
}

fn random_city() -> String {
    pick(CITIES).to_string()
}

fn country() -> (&'static str, &'static str) {
    COUNTRIES
        .choose(&mut rand::rng())
        .copied()
        .unwrap_or(("", ""))
}

fn random_country() -> String {
    country().0.to_string()
}

fn random_country_code() -> String {
    country().1.to_string()
}

fn random_street_name() -> String {
    format!("{} {}", pick(STREET_NAMES), pick(STREET_SUFFIXES))
}

fn random_street_address() -> String {
    format!(
        "{} {}",
        rand::rng().random_range(1..10_000),
        random_street_name()
    )
}

fn random_latitude() -> String {
    format!("{:.6}", rand::rng().random_range(-90.0..=90.0_f64))
}

fn random_longitude() -> String {
    format!("{:.6}", rand::rng().random_range(-180.0..=180.0_f64))
}

fn random_company_name() -> String {
    format!("{} {}", pick(COMPANY_WORDS), pick(COMPANY_SUFFIXES))
}

fn random_job_title() -> String {
    format!(
        "{} {} {}",
        pick(JOB_LEVELS),
        pick(JOB_AREAS),
        pick(JOB_TYPES)
    )
}

fn random_product_name() -> String {
    format!(
        "{} {} {}",
        pick(PRODUCT_ADJECTIVES),
        pick(PRODUCT_MATERIALS),
        pick(PRODUCTS)
    )
}

fn random_price() -> String {
    let cents: u32 = rand::rng().random_range(100..=100_000);
    format!("{}.{:02}", cents / 100, cents % 100)
}

fn random_currency_code() -> String {
    pick(CURRENCIES).to_string()
}

fn random_credit_card_mask() -> String {
    format!("{:04}", rand::rng().random_range(0..10_000))
}

fn random_domain_name() -> String {
    format!("{}-{}.{}", pick(WORDS), pick(WORDS), pick(EMAIL_DOMAINS))
}

fn random_url() -> String {
    format!("https://{}", random_domain_name())
}

fn random_ip() -> String {
    let mut rng = rand::rng();
    format!(
        "{}.{}.{}.{}",
        rng.random_range(1..=254),
        rng.random_range(0..=255),
        rng.random_range(0..=255),
        rng.random_range(1..=254)
    )
}

fn random_ipv6() -> String {
    let mut rng = rand::rng();
    (0..8)
        .map(|_| format!("{:x}", rng.random_range(0..=0xffff_u32)))
        .collect::<Vec<_>>()
        .join(":")
}

fn random_mac_address() -> String {
    let mut rng = rand::rng();
    (0..6)
        .map(|_| format!("{:02x}", rng.random_range(0..=255_u32)))
        .collect::<Vec<_>>()
        .join(":")
}

fn random_image_url() -> String {
    format!(
        "https://picsum.photos/seed/{}/640/480",
        rand::rng().random_range(1..100_000)
    )
}

fn random_color() -> String {
    pick(COLORS).to_string()
}

fn random_hex_color() -> String {
    format!("#{:06x}", rand::rng().random_range(0..=0xff_ffff_u32))
}

fn random_word() -> String {
    pick(WORDS).to_string()
}

fn random_words() -> String {
    let n = rand::rng().random_range(2..=4);
    (0..n).map(|_| pick(WORDS)).collect::<Vec<_>>().join(" ")
}

fn lorem_sentence() -> String {
    let n = rand::rng().random_range(4..=10);
    let words: Vec<&str> = (0..n).map(|_| pick(LOREM)).collect();
    let mut s = words.join(" ");
    if let Some(first) = s.get(..1) {
        s = format!("{}{}", first.to_uppercase(), &s[1..]);
    }
    s.push('.');
    s
}

fn random_lorem_sentence() -> String {
    lorem_sentence()
}

fn random_lorem_paragraph() -> String {
    let n = rand::rng().random_range(3..=6);
    (0..n)
        .map(|_| lorem_sentence())
        .collect::<Vec<_>>()
        .join(" ")
}

const DAY_MS: i64 = 86_400_000;

fn random_date_future() -> String {
    iso_from_millis(now_ms() + rand::rng().random_range(DAY_MS..=365 * DAY_MS))
}

fn random_date_past() -> String {
    iso_from_millis(now_ms() - rand::rng().random_range(DAY_MS..=365 * DAY_MS))
}

fn random_date_recent() -> String {
    iso_from_millis(now_ms() - rand::rng().random_range(0..=DAY_MS))
}

fn random_semver() -> String {
    let mut rng = rand::rng();
    format!(
        "{}.{}.{}",
        rng.random_range(0..10),
        rng.random_range(0..20),
        rng.random_range(0..30)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn is_iso(s: &str) -> bool {
        let b = s.as_bytes();
        s.len() == 24
            && b[4] == b'-'
            && b[7] == b'-'
            && b[10] == b'T'
            && b[13] == b':'
            && b[16] == b':'
            && b[19] == b'.'
            && b[23] == b'Z'
            && s.chars()
                .enumerate()
                .all(|(i, c)| [4, 7, 10, 13, 16, 19, 23].contains(&i) || c.is_ascii_digit())
    }

    fn is_uuid_v4(s: &str) -> bool {
        uuid::Uuid::parse_str(s).is_ok_and(|u| u.get_version_num() == 4) && s == s.to_lowercase()
    }

    fn is_email(s: &str) -> bool {
        let Some((local, domain)) = s.split_once('@') else {
            return false;
        };
        !local.is_empty()
            && domain.contains('.')
            && local.chars().all(|c| c.is_ascii_lowercase() || c == '.')
            && !s.contains(' ')
    }

    fn words(s: &str, min: usize, max: usize) -> bool {
        let n = s.split(' ').count();
        n >= min && n <= max && s.split(' ').all(|w| !w.is_empty())
    }

    fn num_in(s: &str, lo: f64, hi: f64) -> bool {
        s.parse::<f64>().is_ok_and(|v| v >= lo && v <= hi)
    }

    /// The shape every generator's output must have.
    fn shape_ok(name: &str, v: &str) -> bool {
        match name {
            "$guid" | "$uuid" | "$randomUUID" => is_uuid_v4(v),
            "$timestamp" => v.parse::<i64>().is_ok_and(|t| t > 1_600_000_000),
            "$isoTimestamp" | "$randomDateFuture" | "$randomDatePast" | "$randomDateRecent" => {
                is_iso(v)
            }
            "$randomInt" => v.parse::<u32>().is_ok_and(|n| n <= 1000),
            "$randomBoolean" => v == "true" || v == "false",
            "$randomAlphaNumeric" => v.len() == 1 && v.chars().all(|c| c.is_ascii_alphanumeric()),
            "$randomPassword" => v.len() == 15 && v.chars().all(|c| c.is_ascii_alphanumeric()),
            "$randomFirstName" | "$randomLastName" | "$randomCity" => {
                !v.is_empty() && v.chars().next().is_some_and(char::is_uppercase)
            }
            "$randomFullName" => words(v, 2, 2),
            "$randomUserName" => {
                v.contains('.')
                    && v.chars()
                        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '.')
            }
            "$randomEmail" => is_email(v),
            "$randomPhoneNumber" => {
                v.len() == 12
                    && v.chars().enumerate().all(|(i, c)| {
                        if i == 3 || i == 7 {
                            c == '-'
                        } else {
                            c.is_ascii_digit()
                        }
                    })
            }
            "$randomCountry" => COUNTRIES.iter().any(|(n, _)| *n == v),
            "$randomCountryCode" => v.len() == 2 && v.chars().all(|c| c.is_ascii_uppercase()),
            "$randomStreetName" => words(v, 2, 2),
            "$randomStreetAddress" => {
                words(v, 3, 3)
                    && v.split(' ')
                        .next()
                        .is_some_and(|n| n.parse::<u32>().is_ok())
            }
            "$randomLatitude" => num_in(v, -90.0, 90.0),
            "$randomLongitude" => num_in(v, -180.0, 180.0),
            "$randomCompanyName" => words(v, 2, 2),
            "$randomJobTitle" | "$randomProductName" => words(v, 3, 3),
            "$randomPrice" => {
                num_in(v, 1.0, 1000.0) && v.split_once('.').is_some_and(|(_, c)| c.len() == 2)
            }
            "$randomCurrencyCode" => v.len() == 3 && v.chars().all(|c| c.is_ascii_uppercase()),
            "$randomCreditCardMask" => v.len() == 4 && v.chars().all(|c| c.is_ascii_digit()),
            "$randomUrl" | "$randomImageUrl" => {
                url::Url::parse(v).is_ok_and(|u| u.scheme() == "https" && u.host().is_some())
            }
            "$randomDomainName" => v.contains('.') && !v.contains(['/', ' ', ':']),
            "$randomIP" => v.parse::<std::net::Ipv4Addr>().is_ok(),
            "$randomIPV6" => v.parse::<std::net::Ipv6Addr>().is_ok(),
            "$randomMACAddress" => {
                v.len() == 17
                    && v.split(':')
                        .all(|p| p.len() == 2 && u8::from_str_radix(p, 16).is_ok())
            }
            "$randomColor" => COLORS.contains(&v),
            "$randomHexColor" => {
                v.len() == 7 && v.starts_with('#') && u32::from_str_radix(&v[1..], 16).is_ok()
            }
            "$randomWord" => WORDS.contains(&v),
            "$randomWords" => words(v, 2, 4),
            "$randomLoremSentence" => {
                v.ends_with('.')
                    && v.chars().next().is_some_and(char::is_uppercase)
                    && words(v, 4, 10)
            }
            "$randomLoremParagraph" => v.matches('.').count() >= 3,
            "$randomSemver" => {
                v.split('.').count() == 3 && v.split('.').all(|p| p.parse::<u32>().is_ok())
            }
            _ => false,
        }
    }

    #[test]
    fn every_generator_has_the_documented_shape() {
        for g in CATALOGUE {
            assert!(g.name.starts_with('$'), "{}", g.name);
            assert!(
                shape_ok(g.name, g.example),
                "{} example {:?} has the wrong shape",
                g.name,
                g.example
            );
            for _ in 0..50 {
                let v = generate(g.name).unwrap();
                assert!(shape_ok(g.name, &v), "{} produced {v:?}", g.name);
            }
        }
    }

    #[test]
    fn catalogue_is_unique_and_covers_the_postman_core_set() {
        let names: Vec<_> = catalogue().into_iter().map(|c| c.name).collect();
        let mut sorted = names.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), names.len(), "duplicate names");
        for n in [
            "$uuid",
            "$guid",
            "$timestamp",
            "$isoTimestamp",
            "$randomInt",
            "$randomEmail",
            "$randomFullName",
            "$randomIP",
            "$randomIPV6",
            "$randomCreditCardMask",
        ] {
            assert!(is_known(n), "{n}");
        }
        assert!(!is_known("$nope"));
        assert_eq!(generate("$nope"), None);
    }

    #[test]
    fn values_vary() {
        let a: std::collections::HashSet<_> = (0..20).map(|_| generate("$uuid").unwrap()).collect();
        assert_eq!(a.len(), 20);
    }

    #[test]
    fn ctx_memoizes_per_name() {
        let ctx = DynamicCtx::new();
        let a = ctx.value("$uuid").unwrap();
        assert_eq!(ctx.value("$uuid").unwrap(), a);
        assert_ne!(ctx.value("$guid").unwrap(), a);
        assert_eq!(ctx.value("$nope"), None);
        assert_ne!(DynamicCtx::new().value("$uuid").unwrap(), a);
        assert!(DynamicCtx::keep().keeps());
    }

    #[test]
    fn iso_dates() {
        assert_eq!(iso_from_millis(0), "1970-01-01T00:00:00.000Z");
        assert_eq!(iso_from_millis(951_782_400_123), "2000-02-29T00:00:00.123Z");
        assert_eq!(
            iso_from_millis(1_795_773_600_000),
            "2026-11-27T10:00:00.000Z"
        );
        assert_eq!(iso_from_millis(-1), "1969-12-31T23:59:59.999Z");
    }

    /// The UI mock (`ui/src/lib/util/dynamic.ts`) offers the same names.
    #[test]
    fn ui_catalogue_matches() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../ui/src/lib/util/dynamic.ts"
        );
        let Ok(ts) = std::fs::read_to_string(path) else {
            return; // the UI is not part of every checkout (e.g. a crate-only build)
        };
        let ui: Vec<&str> = ts
            .split("name: \"")
            .skip(1)
            .filter_map(|s| s.split('"').next())
            .collect();
        let core: Vec<&str> = CATALOGUE.iter().map(|g| g.name).collect();
        assert_eq!(ui, core);
    }
}
