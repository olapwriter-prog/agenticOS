#!/bin/bash
# Skills, documents and apps through agentd, by chat: the skill library (built by
# scripts/fetch-skills.sh), Word/Excel/PowerPoint/PDF files, and the apps over public APIs with
# agentos-phonesim world standing in for those APIs.
#   scripts/phonesim-skills-apps.sh [SKILL_PACK_DIR] [-v]
set -u
cd "$(dirname "$0")/.."
ROOT=$PWD
PACK=${1:-$ROOT/skillpack}
VERBOSE=${2:-}
W=${PHONESIM_WORK:-/tmp/agentd-skills-apps}
[ -f "$PACK/index.json" ] || { echo "no skill pack at $PACK (run scripts/fetch-skills.sh)"; exit 2; }
cargo build -q -p agentd -p agentos-phonesim || exit 2
AGENTD=$ROOT/target/debug/agentd
SIM=$ROOT/target/debug/agentos-phonesim
PORT=18901
WORLD=http://127.0.0.1:$PORT

pass=0
fail=0
FAILED=""
ok() { pass=$((pass + 1)); echo "  PASS  $*"; }
bad() { fail=$((fail + 1)); FAILED="$FAILED
  $*"; echo "  FAIL  $*"; }
ctl() { $AGENTD ctl -c "$W/agentd.toml" "$@" 2>&1 | tr '\001' '\n'; }
say() {
	local out
	out=$(ctl chat "$2")
	[ -n "$VERBOSE" ] && echo "        > $2" && echo "$out" | head -10 | sed 's/^/          /'
	if echo "$out" | grep -qE -- "$3"; then ok "$1"; else bad "$1: got: $(echo "$out" | head -3 | tr '\n' ' ')"; fi
}

pkill -f "agentos-phonesim world $PORT" 2>/dev/null
rm -rf "$W" && mkdir -p "$W/data/documents"
cp crates/docs/tests/fixtures/* "$W/data/documents/"
$SIM world $PORT >"$W/world.log" 2>&1 &
WPID=$!
for _ in $(seq 1 50); do curl -s "$WORLD/health" >/dev/null && break; sleep 0.1; done
{
	echo "data_dir = \"$W/data\""
	echo 'listen = "127.0.0.1:18770"'
	echo 'kdf_mib = 1'
	echo "skills_dir = \"$PACK\""
	echo '[apps]'
	echo 'home = "Bengaluru"'
	for k in open_meteo geocoding air_quality nominatim osrm frankfurter wikipedia dictionary nager openlibrary radio itunes mealdb mymemory coingecko; do echo "$k = \"$WORLD\""; done
} >"$W/agentd.toml"
export AGENTD_PASSPHRASE=x
$AGENTD init -c "$W/agentd.toml" >/dev/null
$AGENTD run -c "$W/agentd.toml" >"$W/agentd.log" 2>&1 &
APID=$!
for _ in $(seq 1 100); do $AGENTD ctl -c "$W/agentd.toml" status >/dev/null 2>&1 && break; sleep 0.2; done

echo "== skills: 663 guides from Google, Microsoft, OpenAI, Anthropic, Vercel"
say "find the skill for a topic" "skills about deploying a container to google cloud run" "cloud-run-basics: google · Apache-2.0"
say "read the part that answers a question, cut for a 4B model" "skill google/cloud-run-basics: how do I deploy from source" "gcloud run deploy SERVICE_NAME --source"
say "the excerpt keeps within its budget" "skill vercel/vercel-react-best-practices: barrel imports" "of [0-9]+ tokens"
say "a Microsoft SDK guide" "skills for uploading to azure blob storage from python" "azure-storage-blob-py"
say "attribution" "skill sources" "microsoft-docs.*CC-BY-4.0|CC-BY-4.0"

echo "== documents: read natively"
say "the documents on the phone" "my documents" "sales.xlsx \[Excel\]: 2 sheets"
say "a Word clause by question" "what does agreement say about late payment" "1.5% interest per month"
say "an Excel total (a formula's value)" "what does sales.xlsx say about total revenue" "Total \| 360 \| 180000"
say "a slide by question" "what does review.pptx say about next steps" "Open a store in Mysuru"
say "a PDF page" "read document statement.pdf" "Opening balance: 1,50,000.00"

echo "== apps over public APIs (simulated here)"
say "weather at home" "weather" "Weather in Bengaluru"
say "weather elsewhere" "will it rain in Pune" "chance of rain"
say "air quality" "air quality in Bengaluru" "AQI 87"
say "local time" "what time is it in London" "Europe/London"
say "places" "find cafes in Indiranagar nearby" "Third Wave Coffee"
say "directions" "directions from Bengaluru to Mysore Palace" "at the roundabout, take exit 2"
say "currency" "convert 100 dollars to rupees" "8813.70 INR"
say "encyclopedia" "tell me about mysore palace" "Wadiyar dynasty"
say "dictionary" "what does serendipity mean" "pure serendipity"
say "holidays" "public holidays in germany" "Christmas Day"
say "books" "books by narayan" "Malgudi Days"
say "radio" "play jazz radio" "Radio Indigo Jazz"
say "podcasts" "podcasts about gst" "GST Simplified"
say "recipe" "how to make biryani" "Chicken Biryani"
say "translation" "translate good morning to hindi" "सुप्रभात"
say "crypto" "bitcoin price" "INR"
check_screen=$(ctl screen)
if echo "$check_screen" | grep -q '"tool":"crypto"'; then ok "every answer is a screen for the display"; else bad "no screen kept"; fi

echo "== the phone's own tools still win their phrases"
say "contacts still work" "save Mum as +91 98765 43210" "saved Mum"
say "calendar still works" "what's on friday" "nothing|done"

kill $APID $WPID 2>/dev/null
wait $APID 2>/dev/null
echo
echo "$pass passed, $fail failed   (logs: $W/agentd.log)"
[ $fail = 0 ] || { echo "failed:$FAILED"; exit 1; }
