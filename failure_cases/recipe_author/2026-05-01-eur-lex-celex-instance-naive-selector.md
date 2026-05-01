### v1.6 attempt — deferred

v1.6 of the recipe-author prompt shipped the "Hunt the URL
end-to-end" subsection that names the failure mode this case
documents. Verification under EUR-Lex specifically is deferred
to a future session focused on production hardening of edge-
case sources. EUR-Lex's URL behavior (language picker, CELEX
disambiguation, cookie interstitials) is a hard verification
target; the prompt change is shipped and will be re-verified
when EUR-Lex coverage becomes a priority. Friendlier topics
under v1.6 (e.g., "global gold production" against usgs_mcs and
sec_edgar) authored structurally appropriate recipes — the
remaining failures on those topics were runtime gaps
(pdf_table not yet implemented; SEC EDGAR User-Agent format),
not prompt-quality failures.