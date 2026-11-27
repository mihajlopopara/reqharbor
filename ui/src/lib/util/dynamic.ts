/**
 * Dynamic variables (`{{$uuid}}`, `{{$randomEmail}}`, …): the same catalogue
 * as crates/core/src/dynamic.rs (a core test checks the names and order), used
 * by the mock backend to generate values and by the UI for previews.
 *
 * One value per name per send: resolve every field of a request through one
 * `dynamicMemo()`.
 */
import type { DynamicVariableInfo } from "../api/types";

type Gen = () => string;

const FIRST_NAMES = ["Ada", "Alan", "Amara", "Ben", "Carla", "Chen", "Diego", "Elena", "Emma", "Farah", "Grace", "Hana", "Ivan", "Jane", "Jonas", "Kai", "Lena", "Liam", "Maya", "Mateo", "Nia", "Noah", "Olivia", "Omar", "Priya", "Rosa", "Sam", "Sofia", "Tariq", "Yuki", "Zoe"];
const LAST_NAMES = ["Anderson", "Brown", "Costa", "Dubois", "Evans", "Fischer", "Garcia", "Hansen", "Ito", "Jensen", "Kowalski", "Lee", "Martin", "Nakamura", "Novak", "Okafor", "Patel", "Quinn", "Rossi", "Schmidt", "Silva", "Smith", "Tanaka", "Walker", "Weber", "Young", "Zhang"];
const CITIES = ["Amsterdam", "Austin", "Barcelona", "Berlin", "Bogotá", "Cape Town", "Chicago", "Dublin", "Helsinki", "Kyoto", "Lagos", "Lisbon", "London", "Melbourne", "Montreal", "Mumbai", "Nairobi", "Osaka", "Oslo", "Paris", "Prague", "Seoul", "Singapore", "Toronto", "Vienna", "Warsaw"];
const COUNTRIES: [string, string][] = [
  ["Argentina", "AR"], ["Australia", "AU"], ["Austria", "AT"], ["Brazil", "BR"], ["Canada", "CA"], ["Chile", "CL"],
  ["Denmark", "DK"], ["Egypt", "EG"], ["Finland", "FI"], ["France", "FR"], ["Germany", "DE"], ["Ghana", "GH"],
  ["India", "IN"], ["Ireland", "IE"], ["Italy", "IT"], ["Japan", "JP"], ["Kenya", "KE"], ["Mexico", "MX"],
  ["Netherlands", "NL"], ["New Zealand", "NZ"], ["Norway", "NO"], ["Poland", "PL"], ["Portugal", "PT"], ["Serbia", "RS"],
  ["South Korea", "KR"], ["Spain", "ES"], ["Sweden", "SE"], ["United Kingdom", "GB"], ["United States", "US"],
];
const STREET_NAMES = ["Maple", "Oak", "Cedar", "Elm", "Harbor", "Hill", "Lake", "Park", "River", "Station", "Sunset", "Willow", "Church", "Mill", "Bridge", "Spring", "Forest", "Garden"];
const STREET_SUFFIXES = ["Street", "Avenue", "Road", "Lane", "Drive", "Way", "Court", "Place"];
const COMPANY_WORDS = ["Northwind", "Bluefin", "Brightside", "Cobalt", "Evergreen", "Granite", "Horizon", "Ironwood", "Juniper", "Lighthouse", "Meridian", "Nimbus", "Orchid", "Pinnacle", "Quartz", "Redwood", "Silverline", "Summit", "Tidal", "Vertex"];
const COMPANY_SUFFIXES = ["Labs", "Inc", "LLC", "Group", "Systems", "Partners", "Co", "Studios", "Works"];
const JOB_LEVELS = ["Junior", "Senior", "Lead", "Principal", "Chief", "Staff", "Associate"];
const JOB_AREAS = ["Data", "Product", "Security", "Marketing", "Platform", "Design", "Finance", "Operations", "Mobile", "Research", "Support", "Infrastructure"];
const JOB_TYPES = ["Engineer", "Designer", "Manager", "Analyst", "Architect", "Consultant", "Developer", "Specialist", "Strategist", "Coordinator"];
const PRODUCT_ADJECTIVES = ["Ergonomic", "Handcrafted", "Rustic", "Sleek", "Practical", "Refined", "Compact", "Durable", "Elegant", "Gorgeous", "Intelligent", "Modern", "Recycled", "Smart"];
const PRODUCT_MATERIALS = ["Steel", "Wooden", "Cotton", "Granite", "Plastic", "Rubber", "Bamboo", "Concrete", "Leather", "Glass", "Bronze", "Ceramic"];
const PRODUCTS = ["Chair", "Table", "Lamp", "Keyboard", "Mug", "Backpack", "Bottle", "Clock", "Desk", "Jacket", "Shoes", "Speaker", "Wallet", "Watch", "Headphones"];
const CURRENCIES = ["USD", "EUR", "GBP", "JPY", "CHF", "CAD", "AUD", "NZD", "SEK", "NOK", "DKK", "PLN", "CZK", "RSD", "BRL", "MXN", "INR", "CNY", "KRW", "SGD", "ZAR"];
const COLORS = ["red", "orange", "yellow", "green", "teal", "cyan", "blue", "indigo", "violet", "purple", "magenta", "pink", "brown", "black", "white", "gray", "silver", "gold", "olive", "maroon", "navy", "lime", "coral", "salmon"];
const WORDS = ["amber", "anchor", "arrow", "aurora", "beacon", "bloom", "breeze", "canyon", "cascade", "cedar", "comet", "coral", "crystal", "delta", "ember", "falcon", "fern", "glacier", "harbor", "island", "jade", "lagoon", "lantern", "maple", "meadow", "nebula", "ocean", "orbit", "pebble", "prairie", "quartz", "quiet", "river", "sable", "signal", "summit", "tide", "velvet", "willow", "zephyr"];
const LOREM = ["lorem", "ipsum", "dolor", "sit", "amet", "consectetur", "adipiscing", "elit", "sed", "do", "eiusmod", "tempor", "incididunt", "ut", "labore", "et", "dolore", "magna", "aliqua", "enim", "ad", "minim", "veniam", "quis", "nostrud", "exercitation", "ullamco", "laboris", "nisi", "aliquip", "ex", "ea", "commodo", "consequat", "duis", "aute", "irure", "in", "reprehenderit", "voluptate", "velit", "esse", "cillum", "fugiat", "nulla", "pariatur"];
const EMAIL_DOMAINS = ["example.com", "example.org", "example.net"];
const ALNUM = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
const DAY_MS = 86_400_000;

/** Integer in [lo, hi] (inclusive). */
const int = (lo: number, hi: number): number => lo + Math.floor(Math.random() * (hi - lo + 1));
const pick = <T>(list: readonly T[]): T => list[int(0, list.length - 1)];
const pad = (n: number, w: number): string => String(n).padStart(w, "0");
const alnum = (n: number): string => Array.from({ length: n }, () => ALNUM[int(0, ALNUM.length - 1)]).join("");
const hex = (n: number, w = 1): string => n.toString(16).padStart(w, "0");

function uuid(): string {
  if (typeof crypto !== "undefined" && "randomUUID" in crypto) return crypto.randomUUID();
  const h = Array.from({ length: 32 }, () => hex(int(0, 15)));
  h[12] = "4";
  h[16] = hex((int(0, 15) & 0x3) | 0x8);
  const s = h.join("");
  return `${s.slice(0, 8)}-${s.slice(8, 12)}-${s.slice(12, 16)}-${s.slice(16, 20)}-${s.slice(20)}`;
}
const iso = (ms: number): string => new Date(ms).toISOString();
const streetName = (): string => `${pick(STREET_NAMES)} ${pick(STREET_SUFFIXES)}`;
const domain = (): string => `${pick(WORDS)}-${pick(WORDS)}.${pick(EMAIL_DOMAINS)}`;
function sentence(): string {
  const s = Array.from({ length: int(4, 10) }, () => pick(LOREM)).join(" ");
  return `${s[0].toUpperCase()}${s.slice(1)}.`;
}
function price(): string {
  const cents = int(100, 100_000);
  return `${Math.floor(cents / 100)}.${pad(cents % 100, 2)}`;
}

/** Name (with `$`), description, example, generator — same order as core. */
const CATALOGUE: (DynamicVariableInfo & { gen: Gen })[] = [
  { name: "$guid", description: "A v4 UUID", example: "611c2e81-2ccb-42d8-9ddc-2d0bfa65c1b4", gen: uuid },
  { name: "$uuid", description: "A v4 UUID", example: "3e6a4f26-8e0b-4b1c-9a35-0f1d7f0c2a9e", gen: uuid },
  { name: "$randomUUID", description: "A v4 UUID", example: "6929bb52-3ab2-448a-9796-d6480ecad36b", gen: uuid },
  { name: "$timestamp", description: "Current Unix time in seconds", example: "1795773600", gen: () => String(Math.floor(Date.now() / 1000)) },
  { name: "$isoTimestamp", description: "Current time, ISO 8601 (UTC)", example: "2026-11-27T10:00:00.000Z", gen: () => iso(Date.now()) },
  { name: "$randomInt", description: "Random integer 0–1000", example: "482", gen: () => String(int(0, 1000)) },
  { name: "$randomBoolean", description: "Random true or false", example: "true", gen: () => String(Math.random() < 0.5) },
  { name: "$randomAlphaNumeric", description: "Random letter or digit", example: "k", gen: () => alnum(1) },
  { name: "$randomPassword", description: "Random 15-character password", example: "t9iXe7COoDKv8k3", gen: () => alnum(15) },
  { name: "$randomFirstName", description: "Random first name", example: "Jane", gen: () => pick(FIRST_NAMES) },
  { name: "$randomLastName", description: "Random last name", example: "Doe", gen: () => pick(LAST_NAMES) },
  { name: "$randomFullName", description: "Random full name", example: "Jane Doe", gen: () => `${pick(FIRST_NAMES)} ${pick(LAST_NAMES)}` },
  { name: "$randomUserName", description: "Random user name", example: "jane.doe42", gen: () => `${pick(FIRST_NAMES).toLowerCase()}.${pick(LAST_NAMES).toLowerCase()}${int(1, 99)}` },
  { name: "$randomEmail", description: "Random email", example: "jane.doe@example.com", gen: () => `${pick(FIRST_NAMES).toLowerCase()}.${pick(LAST_NAMES).toLowerCase()}@${pick(EMAIL_DOMAINS)}` },
  { name: "$randomPhoneNumber", description: "Random phone number", example: "555-201-7788", gen: () => `555-${pad(int(200, 999), 3)}-${pad(int(0, 9999), 4)}` },
  { name: "$randomCity", description: "Random city", example: "Lisbon", gen: () => pick(CITIES) },
  { name: "$randomCountry", description: "Random country", example: "Portugal", gen: () => pick(COUNTRIES)[0] },
  { name: "$randomCountryCode", description: "Random ISO 3166 country code", example: "PT", gen: () => pick(COUNTRIES)[1] },
  { name: "$randomStreetName", description: "Random street name", example: "Maple Avenue", gen: streetName },
  { name: "$randomStreetAddress", description: "Random street address", example: "742 Maple Avenue", gen: () => `${int(1, 9999)} ${streetName()}` },
  { name: "$randomLatitude", description: "Random latitude", example: "38.722252", gen: () => (Math.random() * 180 - 90).toFixed(6) },
  { name: "$randomLongitude", description: "Random longitude", example: "-9.139337", gen: () => (Math.random() * 360 - 180).toFixed(6) },
  { name: "$randomCompanyName", description: "Random company name", example: "Northwind Labs", gen: () => `${pick(COMPANY_WORDS)} ${pick(COMPANY_SUFFIXES)}` },
  { name: "$randomJobTitle", description: "Random job title", example: "Senior Data Engineer", gen: () => `${pick(JOB_LEVELS)} ${pick(JOB_AREAS)} ${pick(JOB_TYPES)}` },
  { name: "$randomProductName", description: "Random product name", example: "Ergonomic Steel Chair", gen: () => `${pick(PRODUCT_ADJECTIVES)} ${pick(PRODUCT_MATERIALS)} ${pick(PRODUCTS)}` },
  { name: "$randomPrice", description: "Random price 1.00–1000.00", example: "249.99", gen: price },
  { name: "$randomCurrencyCode", description: "Random ISO 4217 currency code", example: "EUR", gen: () => pick(CURRENCIES) },
  { name: "$randomCreditCardMask", description: "Random last four card digits", example: "4821", gen: () => pad(int(0, 9999), 4) },
  { name: "$randomUrl", description: "Random URL", example: "https://bright-harbor.example.com", gen: () => `https://${domain()}` },
  { name: "$randomDomainName", description: "Random domain name", example: "bright-harbor.example.com", gen: domain },
  { name: "$randomIP", description: "Random IPv4 address", example: "203.0.113.42", gen: () => `${int(1, 254)}.${int(0, 255)}.${int(0, 255)}.${int(1, 254)}` },
  { name: "$randomIPV6", description: "Random IPv6 address", example: "2001:db8:85a3:8d3:1319:8a2e:370:7348", gen: () => Array.from({ length: 8 }, () => hex(int(0, 0xffff))).join(":") },
  { name: "$randomMACAddress", description: "Random MAC address", example: "3c:22:fb:9e:41:07", gen: () => Array.from({ length: 6 }, () => hex(int(0, 255), 2)).join(":") },
  { name: "$randomImageUrl", description: "Random 640×480 image URL", example: "https://picsum.photos/seed/4821/640/480", gen: () => `https://picsum.photos/seed/${int(1, 99_999)}/640/480` },
  { name: "$randomColor", description: "Random colour name", example: "teal", gen: () => pick(COLORS) },
  { name: "$randomHexColor", description: "Random hex colour", example: "#1f9ede", gen: () => `#${hex(int(0, 0xffffff), 6)}` },
  { name: "$randomWord", description: "Random word", example: "harbor", gen: () => pick(WORDS) },
  { name: "$randomWords", description: "Random words", example: "quiet amber river", gen: () => Array.from({ length: int(2, 4) }, () => pick(WORDS)).join(" ") },
  { name: "$randomLoremSentence", description: "Random lorem ipsum sentence", example: "Dolor sit amet consectetur.", gen: sentence },
  { name: "$randomLoremParagraph", description: "Random lorem ipsum paragraph", example: "Lorem ipsum dolor sit amet. Sed do eiusmod tempor. Ut enim ad minim veniam.", gen: () => Array.from({ length: int(3, 6) }, sentence).join(" ") },
  { name: "$randomDateFuture", description: "Random date in the next year, ISO 8601", example: "2027-03-14T08:21:37.000Z", gen: () => iso(Date.now() + int(DAY_MS, 365 * DAY_MS)) },
  { name: "$randomDatePast", description: "Random date in the last year, ISO 8601", example: "2026-02-03T17:45:12.000Z", gen: () => iso(Date.now() - int(DAY_MS, 365 * DAY_MS)) },
  { name: "$randomDateRecent", description: "Random date in the last day, ISO 8601", example: "2026-11-26T22:10:05.000Z", gen: () => iso(Date.now() - int(0, DAY_MS)) },
  { name: "$randomSemver", description: "Random semantic version", example: "2.14.3", gen: () => `${int(0, 9)}.${int(0, 19)}.${int(0, 29)}` },
];

const BY_NAME = new Map(CATALOGUE.map((c) => [c.name, c]));

/** The catalogue as `get_dynamic_variables` returns it. */
export function dynamicCatalogue(): DynamicVariableInfo[] {
  return CATALOGUE.map(({ name, description, example }) => ({ name, description, example }));
}

/** A fresh value for `name` (with `$`), or `undefined` when unknown. */
export function generateDynamic(name: string): string | undefined {
  return BY_NAME.get(name)?.gen();
}

/** One value per name: call it for every field of one send. */
export function dynamicMemo(): (name: string) => string | undefined {
  const memo = new Map<string, string>();
  return (name) => {
    const hit = memo.get(name);
    if (hit !== undefined) return hit;
    const v = generateDynamic(name);
    if (v !== undefined) memo.set(name, v);
    return v;
  };
}
