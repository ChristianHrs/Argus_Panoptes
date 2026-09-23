-- First pass: pre-emptive coverage.
--
--   sqlite3 spending.db < seeds/rules_01.sql
--   cargo run -- categorise
--
-- This file guesses ahead, covering chains you plausibly use in
-- London, Haute-Savoie, Kraków and online, so most new transactions land
-- somewhere sensible without another round trip.
--
-- PRIORITY 95, deliberately. Every hand-written rule in rules.sql through
-- rules_04 sits at 10-90 and therefore wins. These only fill gaps. A few
-- riskier patterns sit at 98 so they lose to everything, including each
-- other in the order written.
--
-- Accents are spelled around throughout: SQLite's LIKE folds ASCII case
-- only, so 'é' must match byte for byte. '%Intermarch%' beats
-- '%Intermarché%' every time.
--
-- Expect some of these to be wrong for you. Check what lands where after the
-- first run, and delete the ones that misfire:
--
--   DELETE FROM category_rules WHERE pattern = '%WHATEVER%';


------------------------------------------------------------------------------
-- GROCERIES
--
-- UK, French and Polish chains. '%Casino%' is a French supermarket group but
-- also the obvious other thing — check it if any appear.
------------------------------------------------------------------------------
INSERT OR IGNORE INTO category_rules (category_id, pattern, match_field, priority)
SELECT c.id, v.column1, 'description', 95
FROM categories c
JOIN (VALUES
        ('%ASDA%'), ('%MORRISON%'), ('%ICELAND%'), ('%OCADO%'),
        ('%WHOLE FOODS%'), ('%PLANET ORGANIC%'), ('%COSTCUTTER%'),
        ('%SPAR%'), ('%BUDGENS%'), ('%LONDIS%'), ('%PREMIER STORE%'),
        ('%FARMFOODS%'), ('%POUNDLAND%'),
        -- France
        ('%CARREFOUR%'), ('%MONOPRIX%'), ('%INTERMARCH%'), ('%AUCHAN%'),
        ('%FRANPRIX%'), ('%LECLERC%'), ('%SUPER U%'), ('%PICARD%'),
        ('%GRAND FRAIS%'), ('%NATURALIA%'),
        -- Poland
        ('%BIEDRONKA%'), ('%ZABKA%'), ('%KAUFLAND%')
       ) AS v
WHERE c.name = 'Groceries';


------------------------------------------------------------------------------
-- COFFEE
------------------------------------------------------------------------------
INSERT OR IGNORE INTO category_rules (category_id, pattern, match_field, priority)
SELECT c.id, v.column1, 'description', 95
FROM categories c
JOIN (VALUES
        ('%COSTA%'), ('%STARBUCKS%'), ('%CAFFE NERO%'), ('%CAFE NERO%'),
        ('%BLANK STREET%'), ('%WATCHHOUSE%'), ('%MONMOUTH%'),
        ('%DEPARTMENT OF COFFEE%'), ('%BLACK SHEEP COFFEE%'),
        ('%JOE %26 THE JUICE%'), ('%GRIND%'), ('%ESPRESSO%'),
        ('%COFFEE%'), ('%CAFE%'), ('%BOULANGERIE%'), ('%PATISSERIE%')
       ) AS v
WHERE c.name = 'Coffee';


------------------------------------------------------------------------------
-- FAST FOOD
------------------------------------------------------------------------------
INSERT OR IGNORE INTO category_rules (category_id, pattern, match_field, priority)
SELECT c.id, v.column1, 'description', 95
FROM categories c
JOIN (VALUES
        ('%KFC%'), ('%SUBWAY%'), ('%DOMINO%'), ('%PAPA JOHN%'),
        ('%PIZZA HUT%'), ('%FIVE GUYS%'), ('%WINGSTOP%'), ('%POPEYES%'),
        ('%TACO BELL%'), ('%SHAKE SHACK%'), ('%CHIPOTLE%'),
        ('%GERMAN DONER%'), ('%CHICKEN SHOP%'), ('%FISH %26 CHIP%'),
        ('%ITSU%'), ('%LEON RESTAURANT%'), ('%PIZZA%'), ('%DONER%')
       ) AS v
WHERE c.name = 'Fast food';


------------------------------------------------------------------------------
-- DRINKS
--
-- Pubs rarely have "pub" in the name, so this leans on chains and on the
-- naming conventions that actually show up: "The X Arms", "X Tavern".
-- '%BAR%' is deliberately absent — it matches BARCLAYS and BARBICAN.
------------------------------------------------------------------------------
INSERT OR IGNORE INTO category_rules (category_id, pattern, match_field, priority)
SELECT c.id, v.column1, 'description', 95
FROM categories c
JOIN (VALUES
        ('%WETHERSPOON%'), ('%BREWDOG%'), ('%GREENE KING%'),
        ('%YOUNGS%'), ('%FULLERS%'), ('%NICHOLSON%'), ('%STONEGATE%'),
        ('%SLUG %26 LETTUCE%'), ('%ALL BAR ONE%'), ('%BE AT ONE%'),
        ('%DIRTY MARTINI%'), ('%SIMMONS%'),
        ('% ARMS%'), ('%TAVERN%'), ('%BREWERY%'), ('%BREWING%'),
        ('%TAPROOM%'), ('%WINE BAR%'), ('%COCKTAIL%'), ('%DISTILLERY%')
       ) AS v
WHERE c.name = 'Drinks';


------------------------------------------------------------------------------
-- EATING OUT
--
-- Sit-down chains, plus the delivery apps. Delivery could arguably be Fast
-- food; it is here because what arrives is usually a restaurant meal.
------------------------------------------------------------------------------
INSERT OR IGNORE INTO category_rules (category_id, pattern, match_field, priority)
SELECT c.id, v.column1, 'description', 95
FROM categories c
JOIN (VALUES
        ('%NANDO%'), ('%WAGAMAMA%'), ('%PIZZA EXPRESS%'), ('%FRANCO MANCA%'),
        ('%DISHOOM%'), ('%HONEST BURGER%'), ('%BYRON%'), ('%GBK%'),
        ('%TGI%'), ('%ZIZZI%'), ('%ASK ITALIAN%'), ('%CARLUCCIO%'),
        ('%YO! SUSHI%'), ('%PHO %'), ('%BIBIMBAP%'), ('%BRASSERIE%'),
        ('%BISTRO%'), ('%RESTAURANT%'), ('%TRATTORIA%'), ('%TAQUERIA%'),
        ('%CREPERIE%'), ('%KITCHEN%'), ('%GRILL%'), ('%DINER%')
       ) AS v
WHERE c.name = 'Eating out';


------------------------------------------------------------------------------
-- TRANSPORT
--
-- '%SHELL%' is fuel here. If you ever switch to Shell Energy for the flat,
-- add a '%SHELL ENERGY%' rule at a lower number so it wins.
------------------------------------------------------------------------------
INSERT OR IGNORE INTO category_rules (category_id, pattern, match_field, priority)
SELECT c.id, v.column1, 'description', 95
FROM categories c
JOIN (VALUES
        -- Rail
        ('%NATIONAL RAIL%'), ('%SOUTHEASTERN%'), ('%THAMESLINK%'),
        ('%SOUTHERN RAIL%'), ('%LNER%'), ('%AVANTI%'), ('%GWR%'),
        ('%GREAT WESTERN%'), ('%SNCF%'), ('%OUIGO%'), ('%RAILCARD%'),
        -- Road and hire
        ('%BOLT.EU%'), ('%FREENOW%'), ('%ADDISON LEE%'), ('%GETT%'),
        ('%ZIPCAR%'), ('%ENTERPRISE RENT%'), ('%HERTZ%'), ('%SIXT%'),
        ('%EUROPCAR%'), ('%BLABLACAR%'),
        -- Micromobility
        ('%LIME%'), ('%FOREST BIKE%'), ('%VOI %'), ('%DOTT%'),
        -- Fuel and parking
        ('%SHELL%'), ('%ESSO%'), ('%TEXACO%'), ('%APPLEGREEN%'),
        ('%BP CONNECT%'), ('%MOTO SERVICE%'), ('%Q-PARK%'), ('%NCP %'),
        ('%RINGGO%'), ('%PARKING%'), ('%CONGESTION CHARGE%'),
        ('%DART CHARGE%'), ('%DVLA%'), ('%AUTOROUTE%'), ('%PEAGE%')
       ) AS v
WHERE c.name = 'Transport';


------------------------------------------------------------------------------
-- TRAVEL
--
-- Flights and accommodation, as distinct from getting around day to day.
------------------------------------------------------------------------------
INSERT OR IGNORE INTO category_rules (category_id, pattern, match_field, priority)
SELECT c.id, v.column1, 'description', 95
FROM categories c
JOIN (VALUES
        ('%AIRBNB%'), ('%EXPEDIA%'), ('%HOTELS.COM%'), ('%AGODA%'),
        ('%TRIP.COM%'), ('%HOSTELWORLD%'), ('%SKYSCANNER%'),
        ('%RYANAIR%'), ('%EASYJET%'), ('%BRITISH AIRWAYS%'), ('%WIZZ%'),
        ('%VUELING%'), ('%AIR FRANCE%'), ('%KLM%'), ('%LUFTHANSA%'),
        ('%TRANSAVIA%'), ('%EUROTUNNEL%'), ('%BRITTANY FERRIES%'),
        ('%TRAVELODGE%'), ('%PREMIER INN%'), ('%HILTON%'), ('%MARRIOTT%'),
        ('%NOVOTEL%'), ('%IBIS%'), ('%ACCOR%'), ('%HOSTEL%'),
        ('%TRAVELEX%'), ('%VISA APPLICATION%'), ('%PASSPORT OFFICE%')
       ) AS v
WHERE c.name = 'Travel';


------------------------------------------------------------------------------
-- BILLS & UTILITIES
------------------------------------------------------------------------------
INSERT OR IGNORE INTO category_rules (category_id, pattern, match_field, priority)
SELECT c.id, v.column1, 'description', 95
FROM categories c
JOIN (VALUES
        -- Energy and water
        ('%BRITISH GAS%'), ('%EDF%'), ('%SCOTTISH POWER%'), ('%OVO ENERGY%'),
        ('%UTILITA%'), ('%SHELL ENERGY%'), ('%ENGIE%'), ('%VEOLIA%'),
        ('%SOUTHERN WATER%'), ('%ANGLIAN WATER%'), ('%WATER PLC%'),
        -- Broadband, TV, mobile
        ('%VIRGIN MEDIA%'), ('%BT GROUP%'), ('%BT BROADBAND%'),
        ('%SKY BROADBAND%'), ('%SKY UK%'), ('%TALKTALK%'), ('%PLUSNET%'),
        ('%HYPEROPTIC%'), ('%COMMUNITY FIBRE%'), ('%GIFFGAFF%'),
        ('%LEBARA%'), ('%LYCAMOBILE%'), ('%VODAFONE%'), ('%O2 UK%'),
        ('%TV LICEN%'), ('%SFR%'), ('%ORANGE FRANCE%'),
        -- Financial admin
        ('%INSURANCE%'), ('%AVIVA%'), ('%ADMIRAL%'), ('%DIRECT LINE%'),
        ('%COUNCIL TAX%'), ('%HMRC%')
       ) AS v
WHERE c.name = 'Bills & utilities';


------------------------------------------------------------------------------
-- SUBSCRIPTIONS
--
-- '%AMAZON PRIME%' is here while plain '%AMAZON%' is Shopping. Prime is more
-- specific but both would match — this one is at 94 so it wins outright
-- rather than relying on rule ordering.
------------------------------------------------------------------------------
INSERT OR IGNORE INTO category_rules (category_id, pattern, match_field, priority)
SELECT c.id, '%AMAZON PRIME%', 'description', 94
FROM categories c WHERE c.name = 'Subscriptions';

INSERT OR IGNORE INTO category_rules (category_id, pattern, match_field, priority)
SELECT c.id, v.column1, 'description', 95
FROM categories c
JOIN (VALUES
        ('%DISNEY%'), ('%YOUTUBE%'), ('%AUDIBLE%'), ('%KINDLE%'),
        ('%PATREON%'), ('%SUBSTACK%'), ('%MEDIUM.COM%'),
        ('%OPENAI%'), ('%CHATGPT%'), ('%ANTHROPIC%'), ('%CLAUDE.AI%'),
        ('%CURSOR%'), ('%NOTION%'), ('%DROPBOX%'), ('%ICLOUD%'),
        ('%GOOGLE STORAGE%'), ('%GOOGLE ONE%'), ('%ADOBE%'),
        ('%JETBRAINS%'), ('%FIGMA%'), ('%CANVA%'), ('%LINKEDIN%'),
        ('%DUOLINGO%'), ('%STRAVA%'), ('%HEADSPACE%'), ('%NORDVPN%'),
        ('%PROTON%'), ('%DIGITALOCEAN%'), ('%VERCEL%'), ('%NETLIFY%'),
        ('%HEROKU%'), ('%NAMECHEAP%'), ('%GODADDY%'), ('%AWS%')
       ) AS v
WHERE c.name = 'Subscriptions';


------------------------------------------------------------------------------
-- HEALTH
--
-- Gyms included. Split them into their own category if the number gets
-- interesting on its own.
------------------------------------------------------------------------------
INSERT OR IGNORE INTO category_rules (category_id, pattern, match_field, priority)
SELECT c.id, v.column1, 'description', 95
FROM categories c
JOIN (VALUES
        ('%SUPERDRUG%'), ('%PHARMAC%'), ('%CHEMIST%'), ('%DENTAL%'),
        ('%DENTIST%'), ('%SPECSAVERS%'), ('%VISION EXPRESS%'),
        ('%OPTICIAN%'), ('%BUPA%'), ('%NUFFIELD%'), ('%ZAVA%'),
        ('%DOCTOLIB%'), ('%HOLLAND %26 BARRETT%'), ('%CLINIC%'),
        -- Gyms and fitness
        ('%PUREGYM%'), ('%THE GYM GROUP%'), ('%FITNESS FIRST%'),
        ('%DAVID LLOYD%'), ('%VIRGIN ACTIVE%'), ('%THIRD SPACE%'),
        ('%CLASSPASS%'), ('%BASECAMP%'), ('%GYM%'), ('%FITNESS%')
       ) AS v
WHERE c.name = 'Health';


------------------------------------------------------------------------------
-- SHOPPING
------------------------------------------------------------------------------
INSERT OR IGNORE INTO category_rules (category_id, pattern, match_field, priority)
SELECT c.id, v.column1, 'description', 95
FROM categories c
JOIN (VALUES
        ('%ASOS%'), ('%ZARA%'), ('%H%26M%'), ('%UNIQLO%'), ('%PRIMARK%'),
        ('%NEXT RETAIL%'), ('%JOHN LEWIS%'), ('%SELFRIDGES%'),
        ('%HARRODS%'), ('%TK MAXX%'), ('%DECATHLON%'), ('%NIKE%'),
        ('%FOOT LOCKER%'), ('%END CLOTHING%'), ('%VINTED%'), ('%DEPOP%'),
        ('%ETSY%'), ('%ALIEXPRESS%'), ('%TEMU%'), ('%SHEIN%'),
        ('%CURRYS%'), ('%RYMAN%'), ('%WATERSTONE%'), ('%ZALANDO%'),
        ('%SEPHORA%'), ('%SPACE NK%'), ('%THE BODY SHOP%'), ('%LUSH%'),
        ('%FNAC%'), ('%DARTY%'), ('%DECATH%')
       ) AS v
WHERE c.name = 'Shopping';


------------------------------------------------------------------------------
-- HOUSEHOLD
------------------------------------------------------------------------------
INSERT OR IGNORE INTO category_rules (category_id, pattern, match_field, priority)
SELECT c.id, v.column1, 'description', 95
FROM categories c
JOIN (VALUES
        ('%B%26Q%'), ('%SCREWFIX%'), ('%WICKES%'), ('%HOMEBASE%'),
        ('%DUNELM%'), ('%ROBERT DYAS%'), ('%WILKO%'), ('%LEROY MERLIN%'),
        ('%BRICO%'), ('%LAUNDR%'), ('%DRY CLEAN%'), ('%PRESSING%')
       ) AS v
WHERE c.name = 'Household';


------------------------------------------------------------------------------
-- ENTERTAINMENT
--
-- '%TATE%' is absent on purpose: it sits inside ESTATE, which would sweep up
-- letting agents. Named galleries instead.
------------------------------------------------------------------------------
INSERT OR IGNORE INTO category_rules (category_id, pattern, match_field, priority)
SELECT c.id, v.column1, 'description', 95
FROM categories c
JOIN (VALUES
        ('%CINEWORLD%'), ('%PICTUREHOUSE%'), ('%CURZON%'), ('%EVERYMAN%'),
        ('%BFI%'), ('%PATHE%'), ('%UGC%'),
        ('%DICE.FM%'), ('%EVENTBRITE%'), ('%SEE TICKETS%'), ('%AXS%'),
        ('%O2 ACADEMY%'), ('%ROUNDHOUSE%'), ('%BARBICAN%'),
        ('%SOUTHBANK%'), ('%NATIONAL THEATRE%'), ('%ROYAL ALBERT%'),
        ('%SADLER%'), ('%TATE MODERN%'), ('%TATE BRITAIN%'),
        ('%MUSEUM%'), ('%GALLERY%'), ('%BOWLING%'), ('%ESCAPE ROOM%'),
        ('%KARTING%'), ('%PADEL%'), ('%STEAMGAMES%'), ('%PLAYSTATION%'),
        ('%NINTENDO%'), ('%XBOX%')
       ) AS v
WHERE c.name = 'Entertainment';


------------------------------------------------------------------------------
-- LEARNING
------------------------------------------------------------------------------
INSERT OR IGNORE INTO category_rules (category_id, pattern, match_field, priority)
SELECT c.id, v.column1, 'description', 95
FROM categories c
JOIN (VALUES
        ('%UDEMY%'), ('%COURSERA%'), ('%EDX%'), ('%PLURALSIGHT%'),
        ('%O%27REILLY%'), ('%MANNING%'), ('%PACKT%'), ('%CODECADEMY%'),
        ('%FRONTEND MASTERS%'), ('%MEETUP%'), ('%CONFERENCE%'),
        ('%WORKSHOP%'), ('%UNIVERSITY%')
       ) AS v
WHERE c.name = 'Learning';


------------------------------------------------------------------------------
-- CASH
------------------------------------------------------------------------------
INSERT OR IGNORE INTO category_rules (category_id, pattern, match_field, priority)
SELECT c.id, v.column1, 'description', 95
FROM categories c
JOIN (VALUES
        ('%CASH WITHDRAWAL%'), ('%ATM %'), ('%LINK ATM%'),
        ('%DISTRIBUTEUR%'), ('%RETRAIT%')
       ) AS v
WHERE c.name = 'Cash';


------------------------------------------------------------------------------
-- FEES
--
-- '%FEE%' is NOT here: it matches COFFEE, which would be a quietly
-- expensive mistake given 135 coffee transactions.
------------------------------------------------------------------------------
INSERT OR IGNORE INTO category_rules (category_id, pattern, match_field, priority)
SELECT c.id, v.column1, 'description', 95
FROM categories c
JOIN (VALUES
        ('%NON-STERLING%'), ('%OVERDRAFT%'), ('%SERVICE CHARGE%'),
        ('%CARD FEE%'), ('%ATM FEE%'), ('%FOREIGN TRANSACTION%'),
        ('%INTEREST CHARGE%'), ('%LATE PAYMENT%')
       ) AS v
WHERE c.name = 'Fees';


------------------------------------------------------------------------------
-- INCOME
------------------------------------------------------------------------------
INSERT OR IGNORE INTO category_rules (category_id, pattern, match_field, priority)
SELECT c.id, v.column1, 'description', 95
FROM categories c
JOIN (VALUES
        ('%SALARY%'), ('%PAYROLL%'), ('%WAGES%'), ('%DIVIDEND%'),
        ('%INTEREST PAID%'), ('%REFUND%'), ('%REIMBURSE%')
       ) AS v
WHERE c.name = 'Income';


------------------------------------------------------------------------------
-- TRANSFERS AND INVESTMENTS
--
-- Priority 20: above the hand-written structural rules at 10, below
-- everything else. Moving money between your own accounts must never be
-- claimed by a merchant pattern.
------------------------------------------------------------------------------
INSERT OR IGNORE INTO category_rules (category_id, pattern, match_field, priority)
SELECT c.id, v.column1, 'description', 20
FROM categories c
JOIN (VALUES
        ('%MONZO%'), ('%STARLING%'), ('%WISE%'), ('%CHASE%'),
        ('%MARCUS BY GOLDMAN%'),   -- NOT '%MARCUS%': that beats
                                   -- "Brother Marcus" at priority 20
        ('%SANTANDER SAVE%'), ('%BARCLAYS%'), ('%HSBC%'),
        ('%NATWEST%'), ('%HALIFAX%'), ('%NATIONWIDE%')
       ) AS v
WHERE c.name = 'Transfers';

INSERT OR IGNORE INTO category_rules (category_id, pattern, match_field, priority)
SELECT c.id, v.column1, 'description', 20
FROM categories c
JOIN (VALUES
        ('%VANGUARD%'), ('%HARGREAVES%'), ('%FREETRADE%'),
        ('%INTERACTIVE INVESTOR%'), ('%COINBASE%'), ('%KRAKEN%'),
        ('%NUTMEG%'), ('%MONEYBOX%'), ('%PENSIONBEE%')
       ) AS v
WHERE c.name = 'Investments';


------------------------------------------------------------------------------
-- After running, check the broad ones actually behaved:
--
--   SELECT category, description, n, spent_gbp
--     FROM (SELECT c.name AS category, t.description,
--                  COUNT(*) n, ROUND(SUM(-t.amount_minor)/100.0,2) spent_gbp
--             FROM transactions t
--             JOIN transaction_categories tc
--               ON tc.account_id = t.account_id AND tc.row_key = t.row_key
--             JOIN category_rules r ON r.id = tc.rule_id
--             JOIN categories c ON c.id = tc.category_id
--            WHERE r.priority >= 94
--            GROUP BY c.name, t.description)
--    ORDER BY category, spent_gbp DESC;
--
-- The ones to look at hardest: '%GRILL%', '%KITCHEN%', '%CAFE%',
-- '%RESTAURANT%', '%GYM%', '%LIME%', '%SHELL%' — all generic enough to catch
-- something unintended.
--
-- One already caught in testing: '%MARCUS%' for the savings bank sat at
-- priority 20 and claimed "Brother Marcus" before the Eating out rule at 90
-- could. LOWER priority wins, so a broad structural pattern outranks every
-- merchant rule. Narrowed to '%MARCUS BY GOLDMAN%'. Worth remembering for
-- anything else added at 10-20.
------------------------------------------------------------------------------

SELECT c.name, COUNT(*) AS rules
FROM category_rules r JOIN categories c ON c.id = r.category_id
GROUP BY c.name ORDER BY rules DESC;
