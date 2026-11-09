-- BDC variable library tables, loaded into the KG duckdb under the `varlib` schema.
--
-- Three inputs, each a teammate's tool or output (pins in varlib.yaml):
--   variable library   dm-bip `extract-variable-library` (linkml/dm-bip#376): per-phv
--                      SingleContinuousVariable / SingleCategoricalVariable entries of
--                      linkml/bdc-variable-library
--   mapping provenance dm-bip `extract-mapping-provenance`: which phvs each HV trans-spec reads
--   concepts           tis-lab/BDC-VarLib's build over the same specs: one slot per concept code
--
-- Nothing here lives in nodes/edges, so the KGX export, verify and coverage are unaffected.
-- Participant-level data (dm-bip mapped-data/) never enters these tables.

CREATE SCHEMA IF NOT EXISTS varlib;

-- One row per input file loaded, so every table can be traced to a repo commit.
CREATE OR REPLACE TABLE varlib.load_manifest (
    input       VARCHAR NOT NULL,  -- variable_library | mapping_provenance | concepts
    path        VARCHAR NOT NULL,
    repo        VARCHAR,
    commit      VARCHAR,           -- for repo checkouts
    sha256      VARCHAR,           -- for generated files
    row_count   BIGINT,
    loaded_at   TIMESTAMP DEFAULT current_timestamp
);

-- dbGaP containment: study (phs) > dataset (pht) > variable (phv)

CREATE OR REPLACE TABLE varlib.study (
    id      VARCHAR PRIMARY KEY,   -- bdchm:Study/phs000280
    phs     VARCHAR,
    cohort  VARCHAR,               -- ARIC, JHS, ... (from the HV <COHORT>-ingest directory)
    name    VARCHAR
);

CREATE OR REPLACE TABLE varlib.dataset (
    pht        VARCHAR PRIMARY KEY,
    study_id   VARCHAR,
    file_name  VARCHAR             -- dbGaP table name, e.g. ANTA
);

-- Per-phv entries. Columns follow bdc-variable-library slot names; the two typed classes
-- share one table because they differ only in which slots are populated.
CREATE OR REPLACE TABLE varlib.variable (
    id                           VARCHAR PRIMARY KEY,  -- dbgap:phv00203151
    phv                          VARCHAR NOT NULL,     -- source_id
    pht                          VARCHAR,              -- file_id
    study_id                     VARCHAR,              -- associated_study
    variable_class               VARCHAR NOT NULL,     -- SingleContinuousVariable | SingleCategoricalVariable
    variable_name                VARCHAR,              -- dbGaP VARNAME, e.g. ANTA01
    source_variable_description  VARCHAR,              -- dbGaP VARDESC
    variable_description         VARCHAR,              -- rendered from spec usages
    variable_label               VARCHAR,
    concept_type                 VARCHAR,              -- ontology anchor, when the emitter fills it
    data_type                    VARCHAR,
    unit                         VARCHAR,
    minimum_value                DOUBLE,
    maximum_value                DOUBLE,
    resolution                   VARCHAR,
    comment                      VARCHAR,
    file_name                    VARCHAR
);

-- coded_values (EnumValue), missing_value (MissingValue), alert_values (AlertValue),
-- kept in document order: dbGaP orders codes meaningfully (Yes/No, severity scales).
CREATE OR REPLACE TABLE varlib.variable_value (
    variable_id        VARCHAR NOT NULL,
    value_kind         VARCHAR NOT NULL,  -- coded | missing | alert
    ordinal            INTEGER NOT NULL,
    indicator_char     VARCHAR,           -- the code, e.g. 1
    indicator_meaning  VARCHAR,           -- the label, e.g. Current smoker
    indicator_type     VARCHAR            -- curie, when present
);

-- Spec-level lineage: one derivation per (spec, dataset) fragment.

CREATE OR REPLACE TABLE varlib.derivation (
    id         VARCHAR PRIMARY KEY,  -- dmcprov:ARIC-ingest/bmi/MeasurementObservation/pht004063
    spec_id    VARCHAR,              -- commit permalink or dmcprov: id
    spec_path  VARCHAR,              -- ARIC-ingest/bmi.yaml (joins concept_source.source_file)
    bdchm_class VARCHAR,             -- the class the fragment derives: MeasurementObservation, Condition, ...
    study_id   VARCHAR,
    pht        VARCHAR,
    name       VARCHAR
);

CREATE OR REPLACE TABLE varlib.derivation_source (
    derivation_id   VARCHAR NOT NULL,
    phv             VARCHAR NOT NULL,
    via_expression  BOOLEAN           -- read only inside an expr, e.g. a participant join key
);

-- Harmonized concepts: one per concept code a BDCHM class uses in the HV specs, from
-- BDC-VarLib's build. Its slots fold the contributing studies, datasets and specs into
-- comma-delimited annotations; they are exploded here.

CREATE OR REPLACE TABLE varlib.concept (
    id            VARCHAR PRIMARY KEY,  -- bdcvl:mondo_0005044 (BDC-VarLib's slot name)
    code          VARCHAR,              -- the concept code from the specs, e.g. MONDO:0005044
    slot_name     VARCHAR NOT NULL,
    category      VARCHAR,              -- BDCHM class, from the module: Condition, MeasurementObservation, ...
    module        VARCHAR,              -- bdc_varlib_condition
    bdchm_class   VARCHAR,
    title         VARCHAR,              -- the code's KG label, else the spec file names
    description   VARCHAR,              -- provenance and method strings from the specs
    range         VARCHAR               -- enum name for coded concepts
);

CREATE OR REPLACE TABLE varlib.concept_mapping (
    concept_id  VARCHAR NOT NULL,
    predicate   VARCHAR NOT NULL,       -- skos:exactMatch, skos:closeMatch, ...
    object_id   VARCHAR NOT NULL        -- OBA:2050068, OMOP:2212186, MONDO:..., HP:...
);

CREATE OR REPLACE TABLE varlib.concept_source (
    concept_id   VARCHAR NOT NULL,
    cohort       VARCHAR,               -- source_study entry
    source_file  VARCHAR                -- ARIC-ingest/albumin_bld.yaml
);

CREATE OR REPLACE TABLE varlib.concept_dataset (
    concept_id  VARCHAR NOT NULL,
    pht         VARCHAR NOT NULL
);

-- phvs the concept annotation lists directly. Often absent ("derived via expr"), which is
-- why varlib.variable_concept also resolves membership through mapping provenance.
CREATE OR REPLACE TABLE varlib.concept_variable_declared (
    concept_id  VARCHAR NOT NULL,
    phv         VARCHAR NOT NULL
);

CREATE OR REPLACE TABLE varlib.enum_value (
    enum_name    VARCHAR NOT NULL,
    value        VARCHAR NOT NULL,
    meaning      VARCHAR,
    description  VARCHAR
);
