"""Pure selected-key validator для blocking coverage workflow contract."""

# re реализует узкую first-party YAML key/indentation модель без external parser dependency.
import re


# Typed ошибка отделяет contract regression от Python/test harness failure.
class CoverageWorkflowContractError(AssertionError):
    """Workflow well-formed text нарушил frozen blocking coverage contract."""


# Fail-closed assertion сохраняет единый exception type и actionable diagnostics.
def _require(condition: bool, message: str) -> None:
    """Отклоняет нарушенный selected-key contract."""

    # False condition никогда не превращается в permissive default.
    if not condition:
        # Message называет конкретного workflow owner-а либо semantic key.
        raise CoverageWorkflowContractError(message)


# Стиль проекта допускает только canonical plain mapping keys в monitored structure.
def _require_canonical_mapping_keys(
    document: str,
    indentation: int,
    context: str,
) -> None:
    """Fail-closed отвергает noncanonical key syntax на выбранном structural уровне."""

    # Canonical key начинается ASCII owner name и имеет colon без separation space.
    canonical_key = re.compile(r"^[A-Za-z_][A-Za-z0-9_-]*:")
    # Проверяются только строки exact structural indentation, не scalar content глубже.
    for line in document.splitlines():
        # Blank lines не создают mapping key.
        if not line.strip():
            continue
        # Tabs только в leading whitespace нарушают constrained indentation style.
        leading_whitespace = line[: len(line) - len(line.lstrip(" \t"))]
        _require("\t" not in leading_whitespace, f"{context}: tab indentation запрещён")
        # Exact leading-space count отделяет owner mapping от child blocks.
        leading_spaces = len(line) - len(line.lstrip(" "))
        # Другой уровень принадлежит соседнему owner-у.
        if leading_spaces != indentation:
            continue
        # Комментарий не создаёт YAML key и разрешён project style-ом.
        structural_text = line[indentation:]
        if structural_text.startswith("#"):
            continue
        # Quoted/explicit/anchored/tagged/merge/spaced-colon keys rejected без interpretation.
        _require(
            canonical_key.match(structural_text) is not None,
            f"{context}: noncanonical mapping key `{structural_text}`",
        )


# Счётчик работает после canonical-style audit и потому не интерпретирует YAML spellings.
def _yaml_key_count(document: str, indentation: int, key: str) -> int:
    """Считает exact canonical plain key на одном structural уровне."""

    # Colon следует сразу после exact owner identity по constrained project style.
    key_pattern = rf"(?m)^{re.escape(' ' * indentation)}{re.escape(key)}:"
    # Caller определяет exact допустимое количество в своей owner mapping.
    return len(re.findall(key_pattern, document))


# Indentation scanner продолжает YAML block через blank lines до реального deindent.
def _indented_block_lines(
    document: str,
    owner_line: str,
    content_indentation: int,
) -> tuple[str, ...]:
    """Возвращает raw block lines до первого непустого deindent."""

    # splitlines сохраняет trailing spaces, важные для shell backslash semantics.
    document_lines = document.splitlines()
    # Exact owner spelling остаётся first-party style contract-ом.
    owner_indexes = tuple(
        line_index
        for line_index, line in enumerate(document_lines)
        if line == owner_line
    )
    # Missing/duplicate owner нельзя разрешать эвристическим выбором первого.
    _require(
        len(owner_indexes) == 1,
        f"workflow обязан содержать один exact owner `{owner_line.strip()}`",
    )
    # Единственный индекс безопасно извлекается после fail-closed проверки.
    owner_index = owner_indexes[0]
    # Prefix отражает checked-in indentation конкретной mapping/block scalar.
    content_prefix = " " * content_indentation
    # Raw lines не нормализуются, чтобы не скрыть shell-significant whitespace.
    block_lines: list[str] = []
    # Только следующие строки до непустого deindent принадлежат owner-у.
    for line in document_lines[owner_index + 1 :]:
        # Blank line не завершает YAML block scalar/mapping.
        if not line.strip():
            block_lines.append(line)
            continue
        # Непустая строка нужной либо большей indentation остаётся внутри block-а.
        if line.startswith(content_prefix):
            block_lines.append(line)
            continue
        # Первый непустой deindent завершает block.
        break
    # Tuple фиксирует порядок и multiplicity для exact caller comparison.
    return tuple(block_lines)


# Named step helper ratchet-ит sequence header и запрещает mapping override имени.
def _named_step_body(coverage_job: str, step_name: str) -> str:
    """Извлекает один exact named step без duplicate `name` mapping key."""

    # Sequence header является единственным допустимым owner-ом public step name.
    step_header = f"      - name: {step_name}\n"
    # Missing/duplicate sequence entries нарушают lifecycle contract одинаково.
    _require(
        coverage_job.count(step_header) == 1,
        f"coverage job обязан содержать один `{step_name}` step",
    )
    # Body заканчивается перед следующим first-party named sequence item.
    step_match = re.search(
        rf"(?ms)^{re.escape(step_header)}(?P<body>.*?)(?=^      - name: |\Z)",
        coverage_job,
    )
    # Упоминание header-а вне expected indentation не считается step-ом.
    _require(step_match is not None, f"не удалось извлечь `{step_name}` step")
    # Narrowing проверен runtime condition-ом выше.
    assert step_match is not None
    # Body нужен scoped key/value assertions.
    step_body = step_match.group("body")
    # Step-level keys используют только canonical plain spelling.
    _require_canonical_mapping_keys(step_body, 8, f"step `{step_name}`")
    # Второй plain/quoted `name` key может override-ить sequence header.
    _require(
        _yaml_key_count(step_body, 8, "name") == 0,
        f"`{step_name}` содержит duplicate name key",
    )
    # Проверенный body возвращается без нормализации.
    return step_body


# Public boundary проверяет только frozen coverage job/update/upload identities.
def validate_coverage_workflow_contract(coverage_workflow: str) -> None:
    """Валидирует exact fail-closed CI wiring stable coverage v2."""

    # Любой workflow/job/step override сериализовал бы normal-concurrency cohort.
    _require(
        re.search(r"(?<![A-Za-z0-9_])RUST_TEST_THREADS(?![A-Za-z0-9_])", coverage_workflow)
        is None,
        "coverage workflow не допускает RUST_TEST_THREADS override",
    )
    # Корневые owner-ы обязаны использовать canonical first-party spelling.
    _require_canonical_mapping_keys(coverage_workflow, 0, "workflow root")
    # Trigger и jobs каждый имеют ровно одного корневого owner-а.
    _require(_yaml_key_count(coverage_workflow, 0, "on") == 1, "workflow обязан иметь один on owner")
    _require(
        _yaml_key_count(coverage_workflow, 0, "jobs") == 1,
        "workflow обязан иметь один jobs owner",
    )
    # Root environment имеет одного owner-а и не допускает hidden concurrency override.
    _require(_yaml_key_count(coverage_workflow, 0, "env") == 1, "workflow обязан иметь один env owner")
    # Root env извлекается отдельно от trigger/jobs mappings.
    root_env_lines = _indented_block_lines(coverage_workflow, "env:", 2)
    # Raw document сохраняет exact first-party indentation для canonical audit-а.
    root_env_document = "\n".join(root_env_lines)
    # Quoted/escaped/explicit environment keys запрещены без YAML decoding.
    _require_canonical_mapping_keys(root_env_document, 2, "workflow root env")
    # Комментарии и blank lines не являются environment entries.
    root_env_entries = tuple(
        stripped_line
        for line in root_env_lines
        if (stripped_line := line.strip()) and not stripped_line.startswith("#")
    )
    # Exact ambient pair фиксирует воспроизводимый Cargo mode без test serialization.
    _require(
        root_env_entries == ('FASTIPLAYER_TEST_SCOPE: hosted', 'CARGO_INCREMENTAL: "0"', "CARGO_TERM_COLOR: always"),
        "workflow root env обязан содержать exact Cargo pair",
    )
    # PR-trigger извлекается только из проверенного canonical `on` owner-а.
    trigger_lines = _indented_block_lines(coverage_workflow, "on:", 2)
    # Отдельный документ сохраняет исходную indentation для scoped key audit-а.
    trigger_document = "\n".join(trigger_lines)
    # Trigger names на уровне on-map также обязаны иметь canonical spelling.
    _require_canonical_mapping_keys(trigger_document, 2, "workflow triggers")
    # Blocking update policy должен реально запускаться на каждом pull request.
    _require(
        _yaml_key_count(trigger_document, 2, "pull_request") == 1,
        "workflow обязан иметь один pull_request trigger",
    )
    # Пустой pull_request map означает отсутствие paths/branches фильтров.
    pull_request_lines = _indented_block_lines(trigger_document, "  pull_request:", 4)
    _require(
        all(
            not (stripped_line := line.strip()) or stripped_line.startswith("#")
            for line in pull_request_lines
        ),
        "pull_request trigger обязан оставаться unfiltered",
    )
    # Jobs-level audit ограничен детьми единственного canonical `jobs` owner-а.
    jobs_lines = _indented_block_lines(coverage_workflow, "jobs:", 2)
    # Raw jobs document сохраняет indentation, нужную следующим scoped checks.
    jobs_document = "\n".join(jobs_lines)
    # Jobs-level keys fail-closed используют только canonical first-party spelling.
    _require_canonical_mapping_keys(jobs_document, 2, "workflow jobs")
    # Duplicate top-level coverage key может shadow-ить checked job.
    _require(
        _yaml_key_count(jobs_document, 2, "coverage") == 1,
        "workflow обязан содержать один semantic coverage job key",
    )
    # Job extraction ограничивает дальнейшие assertions одним public status owner-ом.
    coverage_job_match = re.search(
        r"(?ms)^  coverage:\n(?P<body>.*?)(?=^  [A-Za-z_][A-Za-z0-9_-]*:\n|\Z)",
        jobs_document,
    )
    # Quoted/неfirst-party spelling намеренно rejected exact extraction-ом.
    _require(coverage_job_match is not None, "не удалось извлечь coverage job")
    # Narrowing проверен runtime condition-ом выше.
    assert coverage_job_match is not None
    # Job body сохраняет raw workflow text для scoped selected-key audit-а.
    coverage_job = coverage_job_match.group("body")
    # Coverage job owner keys также обязаны быть canonical plain mappings.
    _require_canonical_mapping_keys(coverage_job, 4, "coverage job")
    # Public status name имеет один owner и exact branch-protection spelling.
    _require(_yaml_key_count(coverage_job, 4, "name") == 1, "coverage job name неоднозначен")
    _require(coverage_job.count("    name: Coverage ratchet\n") == 1, "coverage status name изменён")
    # Coverage job нельзя целиком пропустить либо сделать nonblocking.
    _require(_yaml_key_count(coverage_job, 4, "if") == 0, "coverage job не допускает if override")
    _require(
        _yaml_key_count(coverage_job, 4, "continue-on-error") == 0,
        "coverage job не допускает continue-on-error",
    )
    # Job не зависит от статуса соседнего job и потому не может быть silently skipped.
    _require(_yaml_key_count(coverage_job, 4, "needs") == 0, "coverage job не допускает needs")
    # Empty matrix создаёт ноль job instances, поэтому singular ratchet не имеет strategy.
    _require(
        _yaml_key_count(coverage_job, 4, "strategy") == 0,
        "coverage job не допускает strategy",
    )
    # Job-level env имеет одного owner-а для единственного bounded-artifact override.
    _require(_yaml_key_count(coverage_job, 4, "env") == 1, "coverage job обязан иметь один env owner")
    # Mapping извлекается отдельно, чтобы hidden test-thread override оставался запрещён.
    coverage_job_env_lines = _indented_block_lines(coverage_job, "    env:", 6)
    # Raw document позволяет запретить quoted aliases и duplicate semantic keys.
    coverage_job_env_document = "\n".join(coverage_job_env_lines)
    # Только canonical plain key может управлять размером test-profile artifacts.
    _require_canonical_mapping_keys(coverage_job_env_document, 6, "coverage job env")
    # Комментарии не входят в exact environment inventory.
    coverage_job_env_entries = tuple(
        stripped_line
        for line in coverage_job_env_lines
        if (stripped_line := line.strip()) and not stripped_line.startswith("#")
    )
    # Полный DWARF отключён, а concurrency и coverage flags нельзя подменить через env.
    _require(
        coverage_job_env_entries == ('CARGO_PROFILE_TEST_DEBUG: "0"',),
        "coverage job env обязан содержать только exact bounded-debug override",
    )

    # Единственный steps owner исключает YAML override проверенной sequence.
    _require(_yaml_key_count(coverage_job, 4, "steps") == 1, "coverage steps owner неоднозначен")
    # Sequence извлекается только из проверенного coverage job mapping.
    coverage_step_lines = _indented_block_lines(coverage_job, "    steps:", 6)
    # Raw steps document сохраняет step indentation и shell whitespace.
    coverage_steps = "\n".join(coverage_step_lines)
    # Три lifecycle step-а извлекаются отдельно и ratchet-ят собственные names.
    update_step = _named_step_body(coverage_steps, "Validate baseline update policy")
    measured_step = _named_step_body(coverage_steps, "Run clean coverage suite and ratchet")
    upload_step = _named_step_body(coverage_steps, "Upload coverage report")

    # Measured step обязан выполнить exact blocking owner без conditional suppression.
    _require(_yaml_key_count(measured_step, 8, "run") == 1, "measured run key неоднозначен")
    _require(
        measured_step.count("        run: scripts/coverage.sh check\n") == 1,
        "measured step обязан запускать scripts/coverage.sh check",
    )
    _require(_yaml_key_count(measured_step, 8, "if") == 0, "measured step не допускает if")
    # Measured step наследует проверенные root/job env без дополнительного override.
    _require(_yaml_key_count(measured_step, 8, "env") == 0, "measured step не допускает env")
    _require(
        _yaml_key_count(measured_step, 8, "continue-on-error") == 0,
        "measured step не допускает continue-on-error",
    )
    _require(_yaml_key_count(measured_step, 8, "shell") == 0, "measured shell override запрещён")

    # Update step выполняется ровно для PR, где GitHub предоставляет base ref.
    _require(_yaml_key_count(update_step, 8, "if") == 1, "update if key неоднозначен")
    _require(
        update_step.count("        if: github.event_name == 'pull_request'\n") == 1,
        "update step обязан иметь exact pull_request condition",
    )
    # Base ref принадлежит одному exact env mapping owner-у.
    _require(_yaml_key_count(update_step, 8, "env") == 1, "update env owner неоднозначен")
    update_env_lines = _indented_block_lines(update_step, "        env:", 10)
    # Env child keys проверяются отдельно, не затрагивая shell scalar на том же indent-е.
    update_env_document = "\n".join(update_env_lines)
    _require_canonical_mapping_keys(update_env_document, 10, "coverage update env")
    _require(
        _yaml_key_count(update_step, 10, "COVERAGE_BASE_REF") == 1,
        "COVERAGE_BASE_REF key неоднозначен",
    )
    # Комментарии допустимы в YAML mapping, значения/blank aliases — нет.
    update_env_entries = tuple(
        stripped_line
        for line in update_env_lines
        if (stripped_line := line.strip()) and not stripped_line.startswith("#")
    )
    # Exact base-ref value нельзя направить на head-controlled identity.
    _require(
        update_env_entries == ("COVERAGE_BASE_REF: origin/${{ github.base_ref }}",),
        "update env обязан содержать только exact PR base ref",
    )
    # Update exit нельзя маскировать step-level policy или custom shell-ом без `-e`.
    _require(
        _yaml_key_count(update_step, 8, "continue-on-error") == 0,
        "update continue-on-error запрещён",
    )
    _require(_yaml_key_count(update_step, 8, "shell") == 0, "update shell override запрещён")
    _require(_yaml_key_count(update_step, 8, "run") == 1, "update run key неоднозначен")

    # Scanner сохраняет blank/comment/trailing whitespace внутри shell scalar.
    update_run_lines = _indented_block_lines(update_step, "        run: |", 10)
    # Обе previous policy части читаются до единственного v2 validator invocation.
    previous_baseline_extract = (
        'git show "${COVERAGE_BASE_REF}:coverage/baseline.json" '
        "> /tmp/coverage-previous-baseline.json"
    )
    previous_exceptions_extract = (
        'git show "${COVERAGE_BASE_REF}:coverage/measurement-exceptions.json" '
        "> /tmp/coverage-previous-measurement-exceptions.json"
    )
    update_command = "python3 scripts/coverage_stability.py check-baseline-update"
    update_arguments = (
        "--previous-baseline /tmp/coverage-previous-baseline.json",
        "--previous-measurement-exceptions /tmp/coverage-previous-measurement-exceptions.json",
        "--proposed-baseline coverage/baseline.json",
        "--proposed-measurement-exceptions coverage/measurement-exceptions.json",
        "--identity-migrations coverage/identity-migrations.json",
        "--previous-policy /tmp/coverage-previous-policy.json",
        "--proposed-policy coverage/policy.json",
    )
    # Expected raw content включает two-space shell continuation indentation.
    expected_update_lines = (
        previous_baseline_extract,
        previous_exceptions_extract,
        'git show "${COVERAGE_BASE_REF}:coverage/policy.json" > /tmp/coverage-previous-policy.json',
        f"{update_command} \\",
        *(f"  {argument} \\" for argument in update_arguments[:-1]),
        f"  {update_arguments[-1]}",
    )
    # YAML снимает ровно common ten-space indentation, но не trailing whitespace.
    actual_update_lines = tuple(
        line[10:] if line.startswith(" " * 10) else line for line in update_run_lines
    )
    # Exact tuple запрещает extra commands, blank/comments, broken `\` и exit suffix.
    _require(
        actual_update_lines == expected_update_lines,
        "update run scalar не совпадает с exact fail-closed command tuple",
    )
    # Legacy updater запрещён во всём workflow, а не только в выбранном step-е.
    _require(
        "coverage_metrics.py check-baseline-update" not in coverage_workflow,
        "legacy baseline updater запрещён",
    )

    # Upload выполняется после success/failure и не скрывает собственную ошибку.
    _require(_yaml_key_count(upload_step, 8, "if") == 1, "upload if key неоднозначен")
    _require(upload_step.count("        if: always()\n") == 1, "upload обязан иметь if: always()")
    _require(
        _yaml_key_count(upload_step, 8, "continue-on-error") == 0,
        "upload continue-on-error запрещён",
    )
    _require(_yaml_key_count(upload_step, 8, "shell") == 0, "upload shell override запрещён")
    # Exact action/with owners связывают artifact name с upload boundary.
    _require(_yaml_key_count(upload_step, 8, "uses") == 1, "upload uses key неоднозначен")
    _require(_yaml_key_count(upload_step, 8, "with") == 1, "upload with key неоднозначен")
    upload_action_match = re.search(
        r"(?ms)^        uses: actions/upload-artifact@v4\n"
        r"        with:\n(?P<body>.*)$",
        upload_step,
    )
    _require(upload_action_match is not None, "upload-artifact@v4 with-map отсутствует")
    # Narrowing проверен runtime condition-ом выше.
    assert upload_action_match is not None
    # Artifact name имеет один semantic key и exact public value внутри with-map.
    upload_with_body = upload_action_match.group("body")
    # Upload action inputs являются canonical plain keys внутри scoped with-map.
    _require_canonical_mapping_keys(upload_with_body, 10, "coverage upload with")
    _require(_yaml_key_count(upload_with_body, 10, "name") == 1, "artifact name key неоднозначен")
    _require(
        upload_with_body.count("          name: coverage-report\n") == 1,
        "artifact name обязан быть coverage-report внутри upload step",
    )
